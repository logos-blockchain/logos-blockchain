use lb_blend_proofs::{quota::ProofOfQuota, selection::ProofOfSelection};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes};
use lb_key_management_system_keys::keys::ZkPublicKey;
use nom::{
    IResult,
    error::{Error, ErrorKind},
    number::complete::{le_u16, le_u32, le_u64, u8},
};

use crate::mantle::ops::channel::Ed25519PublicKey;

pub mod array;
pub mod bounded_vec;

pub trait NomEncode {
    // TODO: This could be turned into a `BoundedVec<u8, MAX_BYTES>` if we are
    // always able to set an upper limit on everything that goes through NOM
    // decoding. That would allow us to set an upper bound on ANY nom-encoded
    // struct, including a mantle tx itself.
    fn encode(&self) -> Vec<u8>;
}

pub trait NomDecode: Sized {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self>;
}

impl NomEncode for u8 {
    fn encode(&self) -> Vec<u8> {
        vec![*self]
    }
}

impl NomDecode for u8 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        u8(bytes)
    }
}

impl NomEncode for u16 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u16 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        le_u16(bytes)
    }
}

impl NomEncode for u32 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u32 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        le_u32(bytes)
    }
}

impl NomEncode for u64 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u64 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        le_u64(bytes)
    }
}

// Simple utility to encode a slice of `NomEncode` items by encoding each item
// and concatenating the results. Not implemented on the slice type directly
// `[T]` since that could be misleading.
fn encode_slice<T: NomEncode>(items: &[T]) -> Vec<u8> {
    items.iter().flat_map(NomEncode::encode).collect()
}

impl NomEncode for Fr {
    fn encode(&self) -> Vec<u8> {
        fr_to_bytes(self).encode()
    }
}

impl NomDecode for Fr {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, inner) = <[u8; 32]>::decode(bytes)?;
        Ok((
            remaining_bytes,
            fr_from_bytes(&inner)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}

// Ed25519PublicKey = 32BYTE
impl NomEncode for Ed25519PublicKey {
    fn encode(&self) -> Vec<u8> {
        self.to_bytes().encode()
    }
}

impl NomDecode for Ed25519PublicKey {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, inner) = <[u8; 32]>::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::from_bytes(&inner)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}

impl NomEncode for ZkPublicKey {
    fn encode(&self) -> Vec<u8> {
        self.as_fr().encode()
    }
}

impl NomDecode for ZkPublicKey {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (bytes, inner) = Fr::decode(bytes)?;
        Ok((bytes, Self::new(inner)))
    }
}

impl NomEncode for Epoch {
    fn encode(&self) -> Vec<u8> {
        self.as_ref().encode()
    }
}

impl NomDecode for Epoch {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (bytes, inner) = u32::decode(bytes)?;
        Ok((bytes, Self::new(inner)))
    }
}

impl NomEncode for ProofOfQuota {
    fn encode(&self) -> Vec<u8> {
        <[u8; _]>::from(self).encode()
    }
}

impl NomDecode for ProofOfQuota {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, value) = <[u8; _]>::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}

impl NomEncode for ProofOfSelection {
    fn encode(&self) -> Vec<u8> {
        <[u8; _]>::from(self).encode()
    }
}

impl NomDecode for ProofOfSelection {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, value) = <[u8; _]>::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}
