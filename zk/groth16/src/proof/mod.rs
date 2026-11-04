pub mod deserialize;

use core::fmt::{self, Debug, Formatter};

use ark_bn254::{Bn254, Fq};
use ark_ec::{AffineRepr as _, pairing::Pairing};
use ark_ff::{BigInt, PrimeField as _};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize as _, SerializationError};
use generic_array::{ArrayLength, GenericArray, typenum::Unsigned as _};

pub use crate::proof::deserialize::ProofJsonDeser;
use crate::{
    from_json_error::FromJsonError,
    protocol::Protocol,
    utils::{JsonG1, JsonG2, StringifiedG1, StringifiedG2},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proof<E: Pairing> {
    pub pi_a: E::G1Affine,
    pub pi_b: E::G2Affine,
    pub pi_c: E::G1Affine,
}

pub trait CompressSize: Pairing {
    type G1CompressedSize: ArrayLength;
    type G2CompressedSize: ArrayLength;
}

pub const COMPRESSED_PROOF_SIZE: usize = 128;

const COORDINATE_SIZE: usize = 32;
/// The flags of a compressed point, the two top bits of its last byte.
const FLAGS: u8 = 0b11 << 6;
const INFINITY_FLAG: u8 = 1 << 6;

fn coordinate_limbs(coordinate: &[u8]) -> BigInt<4> {
    let mut limbs = [0u64; 4];
    for (limb, bytes) in limbs.iter_mut().zip(coordinate.chunks_exact(8)) {
        *limb = u64::from_le_bytes(bytes.try_into().expect("a chunk holds 8 bytes"));
    }
    BigInt::new(limbs)
}

fn flagged_coordinate_is_canonical(coordinate: &[u8]) -> bool {
    let last_byte = coordinate[COORDINATE_SIZE - 1];
    if last_byte & INFINITY_FLAG != 0 {
        return false;
    }
    let mut limbs = coordinate_limbs(coordinate);
    limbs.0[3] &= !(u64::from(FLAGS) << 56);
    limbs < Fq::MODULUS
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CompressedProof<E: CompressSize> {
    pub pi_a: GenericArray<u8, E::G1CompressedSize>,
    pub pi_b: GenericArray<u8, E::G2CompressedSize>,
    pub pi_c: GenericArray<u8, E::G1CompressedSize>,
}

impl<E> Debug for CompressedProof<E>
where
    E: CompressSize,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompressedProof")
            .field("pi_a", &hex::encode(&self.pi_a))
            .field("pi_b", &hex::encode(&self.pi_b))
            .field("pi_c", &hex::encode(&self.pi_c))
            .finish()
    }
}

impl<E> Copy for CompressedProof<E>
where
    E: CompressSize,
    GenericArray<u8, E::G1CompressedSize>: Copy,
    GenericArray<u8, E::G2CompressedSize>: Copy,
{
}

impl<E: CompressSize> CompressedProof<E> {
    pub const fn new(
        pi_a: GenericArray<u8, E::G1CompressedSize>,
        pi_b: GenericArray<u8, E::G2CompressedSize>,
        pi_c: GenericArray<u8, E::G1CompressedSize>,
    ) -> Self {
        Self { pi_a, pi_b, pi_c }
    }
}

impl CompressedProof<Bn254> {
    /// Total size = G1 + G2 + G1 at the type level.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; COMPRESSED_PROOF_SIZE] {
        let mut bytes = [0u8; COMPRESSED_PROOF_SIZE];
        let g1 = <Bn254 as CompressSize>::G1CompressedSize::to_usize();
        let g2 = <Bn254 as CompressSize>::G2CompressedSize::to_usize();

        bytes[..g1].copy_from_slice(&self.pi_a);
        bytes[g1..g1 + g2].copy_from_slice(&self.pi_b);
        bytes[g1 + g2..].copy_from_slice(&self.pi_c);

        bytes
    }

    /// Mantle Transaction Encoding: a decoder rejects a point with bit 6 set and
    /// a coordinate not less than the modulus of the base field. This reads the
    /// bytes alone, four comparisons of 256 bits. Whether the points are on
    /// their curve and in their subgroup is left to [`Proof::try_from`], where
    /// the proof is verified and the points decompressed.
    pub fn check_encoding(&self) -> Result<(), SerializationError> {
        let (c0, c1) = self.pi_b.split_at(COORDINATE_SIZE);
        let valid = flagged_coordinate_is_canonical(&self.pi_a)
            && coordinate_limbs(c0) < Fq::MODULUS
            && flagged_coordinate_is_canonical(c1)
            && flagged_coordinate_is_canonical(&self.pi_c);
        if valid {
            Ok(())
        } else {
            Err(SerializationError::InvalidData)
        }
    }

    /// [`Self::from_bytes`], rejecting what [`Self::check_encoding`] rejects.
    pub fn try_from_bytes(bytes: &[u8; COMPRESSED_PROOF_SIZE]) -> Result<Self, SerializationError> {
        let proof = Self::from_bytes(bytes);
        proof.check_encoding()?;
        Ok(proof)
    }

    /// Type-level length bound: accepts exactly G1 + G2 + G1 bytes.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; COMPRESSED_PROOF_SIZE]) -> Self {
        let g1 = <Bn254 as CompressSize>::G1CompressedSize::to_usize();
        let g2 = <Bn254 as CompressSize>::G2CompressedSize::to_usize();

        let mut pi_a = GenericArray::default();
        let mut pi_b = GenericArray::default();
        let mut pi_c = GenericArray::default();

        pi_a.copy_from_slice(&bytes[..g1]);
        pi_b.copy_from_slice(&bytes[g1..g1 + g2]);
        pi_c.copy_from_slice(&bytes[g1 + g2..]);

        Self { pi_a, pi_b, pi_c }
    }
}

impl<E: Pairing> From<&Proof<E>> for ark_groth16::Proof<E> {
    fn from(value: &Proof<E>) -> Self {
        let Proof { pi_a, pi_b, pi_c } = value;
        Self {
            a: *pi_a,
            b: *pi_b,
            c: *pi_c,
        }
    }
}

impl TryFrom<ProofJsonDeser> for Proof<Bn254> {
    type Error = FromJsonError;
    fn try_from(value: ProofJsonDeser) -> Result<Self, Self::Error> {
        if !matches!(value.protocol, Protocol::Groth16) {
            return Err(Self::Error::WrongProtocol(
                value.protocol.as_ref().to_owned(),
            ));
        }
        let ProofJsonDeser {
            pi_a, pi_b, pi_c, ..
        } = value;
        let pi_a = StringifiedG1(pi_a)
            .try_into()
            .map_err(Self::Error::G1PointConversionError)?;
        let pi_b = StringifiedG2(pi_b)
            .try_into()
            .map_err(Self::Error::G2PointConversionError)?;
        let pi_c = StringifiedG1(pi_c)
            .try_into()
            .map_err(Self::Error::G1PointConversionError)?;

        Ok(Self { pi_a, pi_b, pi_c })
    }
}

impl<E: Pairing + CompressSize> TryFrom<&Proof<E>> for CompressedProof<E> {
    type Error = SerializationError;
    fn try_from(value: &Proof<E>) -> Result<Self, SerializationError> {
        let Proof { pi_a, pi_b, pi_c } = value;
        let mut a = GenericArray::default();
        let mut b = GenericArray::default();
        let mut c = GenericArray::default();
        pi_a.serialize_compressed(a.as_mut_slice())?;
        pi_b.serialize_compressed(b.as_mut_slice())?;
        pi_c.serialize_compressed(c.as_mut_slice())?;
        Ok(Self {
            pi_a: a,
            pi_b: b,
            pi_c: c,
        })
    }
}

// Deserialization checks every point is on its curve and in its prime order
// subgroup. The point at infinity has no encoding, and is rejected here.
impl<E: Pairing + CompressSize> TryFrom<&CompressedProof<E>> for Proof<E> {
    type Error = SerializationError;
    fn try_from(value: &CompressedProof<E>) -> Result<Self, SerializationError> {
        let CompressedProof { pi_a, pi_b, pi_c } = value;
        let a = <E::G1Affine as CanonicalDeserialize>::deserialize_compressed(&pi_a[..])?;
        let b = <E::G2Affine as CanonicalDeserialize>::deserialize_compressed(&pi_b[..])?;
        let c = <E::G1Affine as CanonicalDeserialize>::deserialize_compressed(&pi_c[..])?;
        if a.is_zero() || b.is_zero() || c.is_zero() {
            return Err(SerializationError::InvalidData);
        }
        Ok(Self {
            pi_a: a,
            pi_b: b,
            pi_c: c,
        })
    }
}

#[cfg(test)]
mod tests {
    use ark_bn254::{G1Affine, G2Affine};
    use ark_ff::BigInteger as _;

    use super::*;

    fn generators() -> Proof<Bn254> {
        Proof {
            pi_a: G1Affine::generator(),
            pi_b: G2Affine::generator(),
            pi_c: G1Affine::generator(),
        }
    }

    /// Every encoding the check rejects, decompression rejects as well, so the
    /// check only moves the rejection to the decoder.
    #[test]
    fn the_encoding_check_rejects_only_what_decompression_rejects() {
        let modulus: [u8; 32] = Fq::MODULUS.to_bytes_le().try_into().unwrap();
        let real = CompressedProof::<Bn254>::try_from(&generators())
            .unwrap()
            .to_bytes();
        for start in [0, 32, 64, 96] {
            let mut bytes = real;
            bytes[start..start + 32].copy_from_slice(&modulus);
            let proof = CompressedProof::<Bn254>::from_bytes(&bytes);
            assert!(proof.check_encoding().is_err(), "modulus at {start}");
            assert!(
                Proof::<Bn254>::try_from(&proof).is_err(),
                "modulus at {start}"
            );
        }
        for last_byte in [31, 95, 127] {
            let mut bytes = real;
            bytes[last_byte] |= INFINITY_FLAG;
            let proof = CompressedProof::<Bn254>::from_bytes(&bytes);
            assert!(proof.check_encoding().is_err(), "bit 6 at {last_byte}");
            assert!(
                Proof::<Bn254>::try_from(&proof).is_err(),
                "bit 6 at {last_byte}"
            );
        }
    }

    #[test]
    fn a_proof_decodes() {
        let compressed = CompressedProof::<Bn254>::try_from(&generators()).unwrap();
        assert_eq!(Proof::try_from(&compressed).unwrap(), generators());
    }

    #[test]
    fn the_point_at_infinity_is_rejected_at_every_position() {
        let compressed = CompressedProof::<Bn254>::try_from(&generators()).unwrap();
        let mut pi_a = compressed.clone();
        pi_a.pi_a.as_mut_slice().fill(0);
        pi_a.pi_a.as_mut_slice()[31] = 1 << 6;
        let mut pi_b = compressed.clone();
        pi_b.pi_b.as_mut_slice().fill(0);
        pi_b.pi_b.as_mut_slice()[63] = 1 << 6;
        let mut pi_c = compressed;
        pi_c.pi_c.as_mut_slice().fill(0);
        pi_c.pi_c.as_mut_slice()[31] = 1 << 6;
        for proof in [pi_a, pi_b, pi_c] {
            assert!(Proof::<Bn254>::try_from(&proof).is_err());
        }
    }

    #[test]
    fn the_point_at_infinity_is_rejected_whatever_its_coordinate_and_with_both_flags() {
        let compressed = CompressedProof::<Bn254>::try_from(&generators()).unwrap();
        let mut any_coordinate = compressed.clone();
        any_coordinate.pi_a.as_mut_slice().fill(0);
        any_coordinate.pi_a.as_mut_slice()[0] = 5;
        any_coordinate.pi_a.as_mut_slice()[31] = 1 << 6;
        let mut both_flags = compressed;
        both_flags.pi_a.as_mut_slice().fill(0);
        both_flags.pi_a.as_mut_slice()[31] = 0b11 << 6;
        for proof in [any_coordinate, both_flags] {
            assert!(Proof::<Bn254>::try_from(&proof).is_err());
        }
    }
}
