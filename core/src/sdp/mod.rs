pub mod blend;
pub mod locked_notes;

use std::hash::Hash;

use blake2::{Blake2b, Digest as _};
use lb_key_management_system_keys::keys::ZkPublicKey;
use multiaddr::Multiaddr;
use nom::{IResult, Parser as _, bytes::complete::take};
use serde::{Deserialize, Serialize};
use strum::EnumIter;

use crate::{
    block::BlockNumber,
    mantle::{NoteId, ops::channel::Ed25519PublicKey},
    utils::serde_bytes_newtype,
};

pub type SessionNumber = u64;
pub type StakeThreshold = u64;

const ACTIVE_METADATA_BLEND_TYPE: u8 = 0x01;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct MinStake {
    pub threshold: StakeThreshold,
    pub timestamp: BlockNumber,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceParameters {
    pub lock_period: u64,
    pub inactivity_period: u64,
    pub retention_period: u64,
    pub timestamp: BlockNumber,
    pub session_duration: BlockNumber,
}

impl ServiceParameters {
    #[must_use]
    pub const fn session_for_block(&self, block_number: BlockNumber) -> SessionNumber {
        block_number / self.session_duration
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Locator(pub Multiaddr);

impl Locator {
    #[must_use]
    pub const fn new(addr: Multiaddr) -> Self {
        Self(addr)
    }
}

impl AsRef<Multiaddr> for Locator {
    fn as_ref(&self) -> &Multiaddr {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, EnumIter)]
pub enum ServiceType {
    #[serde(rename = "BN")]
    BlendNetwork,
}

impl AsRef<str> for ServiceType {
    fn as_ref(&self) -> &str {
        match self {
            Self::BlendNetwork => "BN",
        }
    }
}

impl From<ServiceType> for usize {
    fn from(service_type: ServiceType) -> Self {
        match service_type {
            ServiceType::BlendNetwork => 0,
        }
    }
}

pub type Nonce = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ProviderId(pub Ed25519PublicKey);

#[derive(Debug)]
pub struct InvalidKeyBytesError;

impl From<Ed25519PublicKey> for ProviderId {
    fn from(pk: Ed25519PublicKey) -> Self {
        Self(pk)
    }
}

impl TryFrom<[u8; 32]> for ProviderId {
    type Error = InvalidKeyBytesError;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        Ed25519PublicKey::from_bytes(&bytes)
            .map(ProviderId)
            .map_err(|_| InvalidKeyBytesError)
    }
}

impl PartialOrd for ProviderId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProviderId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_bytes().cmp(other.0.as_bytes())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct DeclarationId(pub [u8; 32]);
serde_bytes_newtype!(DeclarationId, 32);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub service_type: ServiceType,
    pub provider_id: ProviderId,
    pub locked_note_id: NoteId,
    pub locators: Vec<Locator>,
    pub zk_id: ZkPublicKey,
    pub created: BlockNumber,
    pub active: BlockNumber,
    pub withdrawn: Option<BlockNumber>,
    pub nonce: Nonce,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderInfo {
    pub locators: Vec<Locator>,
    pub zk_id: ZkPublicKey,
}

impl Declaration {
    #[must_use]
    pub fn new(block_number: BlockNumber, declaration_msg: &DeclarationMessage) -> Self {
        Self {
            service_type: declaration_msg.service_type,
            provider_id: declaration_msg.provider_id,
            locked_note_id: declaration_msg.locked_note_id,
            locators: declaration_msg.locators.clone(),
            zk_id: declaration_msg.zk_id,
            created: block_number,
            active: block_number,
            withdrawn: None,
            nonce: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DeclarationMessage {
    pub service_type: ServiceType,
    pub locators: Vec<Locator>,
    pub provider_id: ProviderId,
    pub zk_id: ZkPublicKey,
    pub locked_note_id: NoteId,
}

impl DeclarationMessage {
    #[must_use]
    pub fn id(&self) -> DeclarationId {
        let mut hasher = Blake2b::new();
        let service = match self.service_type {
            ServiceType::BlendNetwork => "BN",
        };

        // From the
        // [spec](https://www.notion.so/nomos-tech/Service-Declaration-Protocol-Specification-1fd261aa09df819ca9f8eb2bdfd4ec1dw):
        // declaration_id = Hash(service||provider_id||zk_id||locators)
        hasher.update(service.as_bytes());
        hasher.update(self.provider_id.0);
        for number in self.zk_id.as_fr().0.0 {
            hasher.update(number.to_le_bytes());
        }
        for locator in &self.locators {
            hasher.update(locator.0.as_ref());
        }

        DeclarationId(hasher.finalize().into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct WithdrawMessage {
    pub declaration_id: DeclarationId,
    pub locked_note_id: NoteId,
    pub nonce: Nonce,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ActiveMessage {
    pub declaration_id: DeclarationId,
    pub nonce: Nonce,
    pub metadata: ActivityMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ActivityMetadata {
    Blend(Box<blend::ActivityProof>),
}

impl ActivityMetadata {
    #[must_use]
    pub fn to_metadata_bytes(&self) -> Vec<u8> {
        match self {
            Self::Blend(proof) => proof.to_metadata_bytes(),
        }
    }

    pub fn from_metadata_bytes(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        if bytes.is_empty() {
            return Err("empty metadata bytes".to_owned().into());
        }

        // Read metadata type byte to determine variant
        let metadata_type = bytes[0];

        match metadata_type {
            ACTIVE_METADATA_BLEND_TYPE => {
                let proof_opt = blend::ActivityProof::from_metadata_bytes(bytes)?;
                Ok(Self::Blend(Box::new(proof_opt)))
            }
            _ => Err(format!("Unknown metadata type: {metadata_type:#x}").into()),
        }
    }
}

fn parse_session_number(input: &[u8]) -> IResult<&[u8], SessionNumber> {
    let (input, bytes) = take(size_of::<SessionNumber>()).parse(input)?;
    let session_bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail)))?;
    Ok((input, SessionNumber::from_le_bytes(session_bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activity_metadata_empty_bytes() {
        let result = ActivityMetadata::from_metadata_bytes(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_activity_metadata_unknown_type() {
        let bytes = vec![0xFF]; // Unknown type
        let result = ActivityMetadata::from_metadata_bytes(&bytes);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unknown metadata type")
        );
    }
}
