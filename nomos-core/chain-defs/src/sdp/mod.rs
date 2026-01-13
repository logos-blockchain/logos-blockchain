pub mod blend;
pub mod da;

use std::hash::Hash;

use blake2::{Blake2b, Digest as _};
use key_management_system_keys::keys::ZkPublicKey;
use multiaddr::Multiaddr;
use nom::{IResult, Parser as _, bytes::complete::take};
use serde::{Deserialize, Serialize};
use strum::EnumIter;

use crate::{
    block::BlockNumber,
    mantle::{NoteId, ops::channel::Ed25519PublicKey},
};

pub type SessionNumber = u64;
pub type StakeThreshold = u64;

const ACTIVE_METADATA_DA_TYPE: u8 = 0x00;
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
    #[serde(rename = "DA")]
    DataAvailability,
}

impl AsRef<str> for ServiceType {
    fn as_ref(&self) -> &str {
        match self {
            Self::BlendNetwork => "BN",
            Self::DataAvailability => "DA",
        }
    }
}

impl From<ServiceType> for usize {
    fn from(service_type: ServiceType) -> Self {
        match service_type {
            ServiceType::BlendNetwork => 0,
            ServiceType::DataAvailability => 1,
        }
    }
}

pub type Nonce = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ProviderId(pub Ed25519PublicKey);

#[derive(Debug)]
pub struct InvalidKeyBytesError;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeclarationId(pub [u8; 32]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActivityId(pub [u8; 32]);

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
            ServiceType::DataAvailability => "DA",
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
    DataAvailability(da::ActivityProof),
    Blend(Box<blend::ActivityProof>),
}

impl ActivityMetadata {
    #[must_use]
    pub fn to_metadata_bytes(&self) -> Vec<u8> {
        match self {
            Self::DataAvailability(proof) => proof.to_metadata_bytes(),
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
            ACTIVE_METADATA_DA_TYPE => {
                let proof_opt = da::ActivityProof::from_metadata_bytes(bytes)?;
                Ok(Self::DataAvailability(proof_opt))
            }
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
    use crate::mantle::encoding::encode_sdp_active;

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

    #[test]
    fn test_active_message_with_metadata() {
        let proof = da::ActivityProof {
            current_session: 100,
            previous_session_opinions: vec![0x01, 0x02],
            current_session_opinions: vec![0x03, 0x04],
        };
        let metadata = ActivityMetadata::DataAvailability(proof);

        let message = ActiveMessage {
            declaration_id: DeclarationId([0xAA; 32]),
            nonce: 42,
            metadata,
        };

        let bytes = encode_sdp_active(&message);

        // Verify structure: declaration_id(32) + nonce(8) + metadata
        assert!(bytes.len() > 40); // At least 32 + 8 + some metadata

        // Verify declaration_id
        assert_eq!(&bytes[..32], &[0xAA; 32]);

        // Verify nonce
        let nonce_bytes: [u8; 8] = bytes[32..40].try_into().unwrap();
        assert_eq!(u64::from_le_bytes(nonce_bytes), 42);
    }
}
