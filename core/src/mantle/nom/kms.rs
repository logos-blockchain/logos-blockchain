use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
use nom::{
    IResult,
    error::{Error, ErrorKind},
};

use crate::mantle::nom::{NomDecode, NomEncode};

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
