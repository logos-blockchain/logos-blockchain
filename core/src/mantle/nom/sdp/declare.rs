use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
use nom::{
    IResult,
    error::{Error, ErrorKind},
};

use crate::{
    mantle::{
        NoteId,
        nom::{NomDecode, NomEncode},
        ops::sdp::SDPDeclareOp,
    },
    sdp::{Locators, ProviderId, ServiceType},
};

impl NomEncode for ServiceType {
    fn encode(&self) -> Vec<u8> {
        <Self as AsRef<u8>>::as_ref(self).encode()
    }
}

impl NomDecode for ServiceType {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, value) = u8::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|()| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}

impl NomEncode for ProviderId {
    fn encode(&self) -> Vec<u8> {
        self.0.encode()
    }
}

impl NomDecode for ProviderId {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (bytes, value) = Ed25519PublicKey::decode(bytes)?;
        Ok((bytes, Self(value)))
    }
}

impl NomEncode for SDPDeclareOp {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.service_type.encode();
        bytes.extend(self.locators.encode());
        bytes.extend(self.provider_id.encode());
        bytes.extend(self.zk_id.encode());
        bytes.extend(self.locked_note_id.encode());
        bytes
    }
}

impl NomDecode for SDPDeclareOp {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (bytes, service_type) = ServiceType::decode(bytes)?;
        let (bytes, locators) = Locators::decode(bytes)?;
        let (bytes, provider_id) = ProviderId::decode(bytes)?;
        let (bytes, zk_id) = ZkPublicKey::decode(bytes)?;
        let (bytes, locked_note_id) = NoteId::decode(bytes)?;

        Ok((
            bytes,
            Self {
                service_type,
                locators,
                provider_id,
                zk_id,
                locked_note_id,
            },
        ))
    }
}
