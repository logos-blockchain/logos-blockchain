use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult, Parser as _,
    combinator::{map, map_res},
};

use crate::{
    mantle::{
        NoteId,
        nom::{NomBoundedVec, NomDecode, NomEncode},
        ops::sdp::SDPDeclareOp,
    },
    sdp::{
        Locator, Locators, MAX_DECLARATION_LOCATOR_COUNT, MAX_LOCATOR_BYTE_SIZE, ProviderId,
        ServiceType,
    },
};

impl NomEncode for ServiceType {
    fn encode(&self) -> Vec<u8> {
        <Self as AsRef<u8>>::as_ref(self).encode()
    }
}

impl NomDecode for ServiceType {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        map_res(u8::decode, Self::try_from).parse(bytes)
    }
}

type NomLocator<'a> = NomBoundedVec<'a, u8, 0, MAX_LOCATOR_BYTE_SIZE, 2>;

impl NomEncode for Locator {
    fn encode(&self) -> Vec<u8> {
        let bounded_bytes =
            BoundedVec::new_unchecked(<Self as AsRef<[u8]>>::as_ref(self).to_owned());
        NomLocator::from(&bounded_bytes).encode()
    }
}

impl NomDecode for Locator {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map_res(NomLocator::decode, Self::try_from).parse(bytes)
    }
}

type NomLocators<'a> = NomBoundedVec<'a, Locator, 1, MAX_DECLARATION_LOCATOR_COUNT, 1>;

impl NomEncode for Locators {
    fn encode(&self) -> Vec<u8> {
        NomLocators::from(self).encode()
    }
}

impl NomDecode for Locators {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(NomLocators::decode, Self::from).parse(bytes)
    }
}

impl NomEncode for ProviderId {
    fn encode(&self) -> Vec<u8> {
        self.0.encode()
    }
}

impl NomDecode for ProviderId {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(Ed25519PublicKey::decode, Self).parse(bytes)
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
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
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
