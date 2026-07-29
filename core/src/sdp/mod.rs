pub mod blend;
pub mod locked_notes;

use core::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};
use std::{collections::HashMap, hash::Hash};

use blake2::{Blake2b, Digest as _};
use bytes::Bytes;
use lb_blake2btree::LeafHash;
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_utils::bounded::{BoundedVec, NonEmptyBoundedVec, UpperBoundedVec};
use multiaddr::{Multiaddr, Protocol};
use nom::{
    IResult,
    error::{Error, ErrorKind},
};
use serde::{Deserialize, Serialize};
use strum::EnumIter;

use crate::{
    block::BlockNumber,
    codec::{self, DeserializeOp as _, SerializeOp as _},
    crypto::Hasher,
    mantle::{
        NoteId,
        ledger::Declarations as ServiceDeclarations,
        nom::{NomCodec, NomDecode, NomEncode},
        ops::channel::Ed25519PublicKey,
    },
    utils::{display_hex_bytes_newtype, serde_bytes_newtype},
};

pub type StakeThreshold = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct MinStake {
    pub threshold: StakeThreshold,
    pub timestamp: BlockNumber,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceParameters {
    /// Maximum epochs during which an activity message must be sent.
    pub inactivity_period: InactivityPeriod,
    // Epoch number at which this parameter was set
    pub epoch: Epoch,
}

pub type NumberOfEpochs = Epoch;

/// Number of epochs without an activity message before a declaration is
/// considered inactive
///
/// Invariant: must be at least [`SNAPSHOT_FINALIZATION_DELAY`].
/// Otherwise, the declaration may be excluded from the active set before
/// the [`Declaration::active`] value (refreshed by an activity message)
/// is reflected in the next snapshot.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "NumberOfEpochs")]
pub struct InactivityPeriod(NumberOfEpochs);

impl InactivityPeriod {
    pub const fn new(period: NumberOfEpochs) -> Result<Self, InactivityPeriodTooSmall> {
        if period.into_inner() < SNAPSHOT_FINALIZATION_DELAY.into_inner() {
            Err(InactivityPeriodTooSmall { period })
        } else {
            Ok(Self(period))
        }
    }

    #[must_use]
    pub const fn into_inner(self) -> NumberOfEpochs {
        self.0
    }
}

impl TryFrom<u32> for InactivityPeriod {
    type Error = InactivityPeriodTooSmall;

    fn try_from(period: u32) -> Result<Self, Self::Error> {
        Self::new(period.into())
    }
}

impl TryFrom<NumberOfEpochs> for InactivityPeriod {
    type Error = InactivityPeriodTooSmall;

    fn try_from(period: NumberOfEpochs) -> Result<Self, Self::Error> {
        Self::new(period)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "inactivity_period must be >= SNAPSHOT_FINALIZATION_DELAY ({SNAPSHOT_FINALIZATION_DELAY:?}); got {period:?}"
)]
pub struct InactivityPeriodTooSmall {
    pub period: NumberOfEpochs,
}

pub const MAX_LOCATOR_BYTE_SIZE: usize = 329;

/// A [`Multiaddr`] whose byte length is bounded to `[0,
/// MAX_LOCATOR_BYTE_SIZE]`.
///
/// The shared `lb_utils::bounded` wrapper enforces the byte-length invariant
/// using `Multiaddr::len()`. `Locator::try_from` performs the additional
/// locator-specific validation below, such as rejecting unspecified, loopback,
/// multicast, documentation, and link-local addresses.
type BoundedMultiaddr = lb_utils::bounded::multiaddr::BoundedMultiaddr<0, MAX_LOCATOR_BYTE_SIZE>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "Multiaddr")]
pub struct Locator(BoundedMultiaddr);

impl Locator {
    #[must_use]
    pub const fn new_unchecked(addr: Multiaddr) -> Self {
        Self(BoundedMultiaddr::new_unchecked(addr))
    }

    #[must_use]
    pub fn into_inner(self) -> Multiaddr {
        self.0.into_inner()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.as_inner().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.as_inner().is_empty()
    }
}

impl AsRef<Multiaddr> for Locator {
    fn as_ref(&self) -> &Multiaddr {
        self.0.as_inner()
    }
}

impl AsRef<[u8]> for Locator {
    fn as_ref(&self) -> &[u8] {
        self.0.as_inner().as_ref()
    }
}

impl TryFrom<Multiaddr> for Locator {
    type Error = String;

    fn try_from(value: Multiaddr) -> Result<Self, Self::Error> {
        BoundedMultiaddr::check_len_against_bounds(value.len())
            .map_err(|e| format!("Invalid multiaddr: {e}"))?;

        for protocol in &value {
            match protocol {
                Protocol::Ip4(ip) if ip.is_unspecified() => {
                    return Err(format!(
                        "Locator multiaddr must not contain an unspecified IPv4 address: {value}"
                    ));
                }
                Protocol::Ip6(ip) if ip.is_unspecified() => {
                    return Err(format!(
                        "Locator multiaddr must not contain an unspecified IPv6 address: {value}"
                    ));
                }
                Protocol::P2p(_) => {
                    return Err(format!(
                        "Locator multiaddr must not contain a peer ID: {value}"
                    ));
                }
                _ => {}
            }
        }

        Ok(Self(BoundedMultiaddr::new_unchecked(value)))
    }
}

impl TryFrom<Vec<u8>> for Locator {
    type Error = String;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        let multiaddr =
            Multiaddr::try_from(value).map_err(|e| format!("Invalid multiaddr: {e}"))?;
        Self::try_from(multiaddr)
    }
}

impl<const MIN: usize, const MAX: usize> TryFrom<BoundedVec<u8, MIN, MAX>> for Locator {
    type Error = String;

    fn try_from(value: BoundedVec<u8, MIN, MAX>) -> Result<Self, Self::Error> {
        const {
            assert!(
                MAX <= MAX_LOCATOR_BYTE_SIZE,
                "Max size cannot be more than the maximum allowed byte size for a locator."
            );
        }
        Self::try_from(value.into_inner())
    }
}

impl FromStr for Locator {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let multiaddr = s
            .parse::<Multiaddr>()
            .map_err(|e| format!("Invalid multiaddr: {e}"))?;
        Self::try_from(multiaddr)
    }
}

impl Display for Locator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl NomEncode for Locator {
    fn encode(&self) -> Vec<u8> {
        let bounded_bytes = UpperBoundedVec::<u8, MAX_LOCATOR_BYTE_SIZE>::new_unchecked(
            <Self as AsRef<[u8]>>::as_ref(self).to_owned(),
        );
        bounded_bytes.encode()
    }
}

impl NomDecode for Locator {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, value) = UpperBoundedVec::<u8, MAX_LOCATOR_BYTE_SIZE>::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
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

impl TryFrom<u8> for ServiceType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::BlendNetwork),
            _ => Err(()),
        }
    }
}

impl AsRef<u8> for ServiceType {
    fn as_ref(&self) -> &u8 {
        match self {
            Self::BlendNetwork => &0,
        }
    }
}

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

// TODO: Remove once the `NomCodec` macro supports logic for custom tags.

#[cfg(test)]
mod service_type_tests {
    use strum::IntoEnumIterator as _;

    use crate::sdp::ServiceType;

    #[test]
    // We make sure the two directions never diverge.
    fn u8_roundtrip() {
        for service_type in ServiceType::iter() {
            let encoded: &u8 = service_type.as_ref();
            let decoded = ServiceType::try_from(*encoded).expect("valid byte");
            assert_eq!(service_type, decoded);
        }
    }
}

pub type Nonce = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, NomCodec)]
pub struct DeclarationId(pub [u8; 32]);
serde_bytes_newtype!(DeclarationId, 32);
display_hex_bytes_newtype!(DeclarationId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub service_type: ServiceType,
    pub provider_id: ProviderId,
    pub locked_note_id: NoteId,
    pub locators: Locators,
    pub zk_id: ZkPublicKey,
    /// The epoch of the block that contained the declaration
    pub created: Epoch,
    /// The latest epoch for which the active message was sent.
    ///
    /// This is used only for checking if the declaration should
    /// be marked as inactive, not for checking if it becomes active.
    /// Idle->Active transition must be handled by the `EpochState`
    /// snapshot logic.
    // TODO: Use Option<Epoch> with a better name.
    pub active: Epoch,
    /// The epoch at which the declaration is scheduled to be withdrawn.
    pub withdraw_at: Option<Epoch>,
    pub nonce: Nonce,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderInfo {
    pub locators: Locators,
    pub zk_id: ZkPublicKey,
}

pub const SNAPSHOT_FINALIZATION_DELAY: Epoch = Epoch::new(2);

impl Declaration {
    #[must_use]
    pub fn new(epoch: Epoch, declaration_msg: &DeclarationMessage) -> Self {
        Self {
            service_type: declaration_msg.service_type,
            provider_id: declaration_msg.provider_id,
            locked_note_id: declaration_msg.locked_note_id,
            locators: declaration_msg.locators.clone(),
            zk_id: declaration_msg.zk_id,
            created: epoch,
            active: epoch.strict_add(SNAPSHOT_FINALIZATION_DELAY),
            withdraw_at: None,
            nonce: 0,
        }
    }

    // A `withdraw_at` that is not set is encoded as zero, over the width its
    // value would take.
    fn info_hash(&self) -> crate::crypto::Hash {
        let mut h = Hasher::new();
        h.update(b"DECLARATION_INFO_HASH_V1");
        h.update([*<ServiceType as AsRef<u8>>::as_ref(&self.service_type)]);
        for locator in &self.locators {
            h.update(locator.0.as_ref());
        }
        h.update(self.provider_id.0);
        for number in self.zk_id.as_fr().0.0 {
            h.update(number.to_le_bytes());
        }
        h.update(self.locked_note_id.as_bytes());
        h.update(self.created.into_inner().to_le_bytes());
        h.update(self.active.into_inner().to_le_bytes());
        h.update(self.withdraw_at.map_or(0, Epoch::into_inner).to_le_bytes());
        h.update(self.nonce.to_le_bytes());
        h.finalize().into()
    }
}

impl LeafHash<DeclarationId> for Declaration {
    fn leaf_hash(&self, declaration_id: &DeclarationId) -> crate::crypto::Hash {
        let mut h = Hasher::new();
        h.update(declaration_id.0);
        h.update(self.info_hash());
        h.finalize().into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Declarations(HashMap<ServiceType, ServiceDeclarations>);

impl Declarations {
    pub fn iter(&self) -> impl Iterator<Item = (&ServiceType, &ServiceDeclarations)> {
        self.0.iter()
    }

    #[must_use]
    pub fn for_service(&self, service_type: &ServiceType) -> Option<&ServiceDeclarations> {
        self.0.get(service_type)
    }
}

impl From<HashMap<ServiceType, ServiceDeclarations>> for Declarations {
    fn from(value: HashMap<ServiceType, ServiceDeclarations>) -> Self {
        Self(value)
    }
}

impl FromIterator<(ServiceType, ServiceDeclarations)> for Declarations {
    fn from_iter<I: IntoIterator<Item = (ServiceType, ServiceDeclarations)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl TryFrom<Bytes> for Declarations {
    type Error = codec::Error;

    fn try_from(bytes: Bytes) -> Result<Self, Self::Error> {
        Self::from_bytes(&bytes)
    }
}

impl TryFrom<Declarations> for Bytes {
    type Error = codec::Error;

    fn try_from(this: Declarations) -> Result<Self, Self::Error> {
        this.to_bytes()
    }
}

pub const MAX_DECLARATION_LOCATOR_COUNT: usize = 8;
pub type Locators = NonEmptyBoundedVec<Locator, MAX_DECLARATION_LOCATOR_COUNT>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
pub struct DeclarationMessage {
    pub service_type: ServiceType,
    pub locators: Locators,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
pub struct WithdrawMessage {
    pub declaration_id: DeclarationId,
    pub nonce: Nonce,
    pub locked_note_id: NoteId,
}

// ActiveMessage = DeclarationId Nonce Metadata — plain field-order concat.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
pub struct ActiveMessage {
    pub declaration_id: DeclarationId,
    pub nonce: Nonce,
    pub metadata: ActivityMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ActivityMetadata {
    Blend(Box<blend::ActivityProof>),
}

const ACTIVE_METADATA_BLEND_TYPE: u8 = 1;

impl NomEncode for ActivityMetadata {
    fn encode(&self) -> Vec<u8> {
        match self {
            Self::Blend(blend_activity_proof) => {
                let mut bytes = vec![ACTIVE_METADATA_BLEND_TYPE];
                bytes.extend(blend_activity_proof.encode());
                bytes
            }
        }
    }
}

impl NomDecode for ActivityMetadata {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, metadata_type) = u8::decode(bytes)?;
        match metadata_type {
            ACTIVE_METADATA_BLEND_TYPE => {
                let (bytes, blend_activity_proof) = blend::ActivityProof::decode(remaining_bytes)?;
                Ok((bytes, Self::Blend(Box::new(blend_activity_proof))))
            }
            _ => Err(nom::Err::Error(Error::new(bytes, ErrorKind::Fail))),
        }
    }
}

// TODO: Remove once the `NomCodec` macro supports logic for custom tags and
// enums.

#[cfg(test)]
mod tests {
    use lb_blake2btree::LeafHash as _;
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};
    use multiaddr::Multiaddr;

    use crate::{
        crypto::{Digest as _, Hash, Hasher},
        sdp::{
            Declaration, DeclarationId, DeclarationMessage, Locator, Locators, ServiceDeclarations,
            ServiceType,
        },
    };

    #[test]
    fn locator_rejects_multiaddr_with_peer_id() {
        assert!("/ip4/65.109.51.37/udp/3000/quic-v1/p2p/12D3KooWL7a8LBbLRYnabptHPFBCmAs49Y7cVMqvzuSdd43tAJk8".parse::<Locator>().unwrap_err().contains("must not contain a peer ID"));
    }

    #[test]
    fn locator_rejects_multiaddr_with_unspecified_ipv4() {
        assert!(
            "/ip4/0.0.0.0/udp/3000/quic-v1"
                .parse::<Locator>()
                .unwrap_err()
                .contains("must not contain an unspecified IPv4 address")
        );
    }

    #[test]
    fn locator_rejects_multiaddr_with_unspecified_ipv6() {
        assert!(
            "/ip6/::/udp/3000/quic-v1"
                .parse::<Locator>()
                .unwrap_err()
                .contains("must not contain an unspecified IPv6 address")
        );
    }

    #[test]
    fn locator_accepts_specific_ip_without_peer_id() {
        let addr: Multiaddr = "/ip4/127.0.0.1/udp/3000/quic-v1".parse().unwrap();

        let result = Locator::try_from(addr.clone()).unwrap();

        assert_eq!(result.into_inner(), addr);
    }

    #[test]
    fn locators_array_serde_equivalence() {
        let locator: Locator = "/ip4/127.0.0.1/udp/3001/quic-v1".parse().unwrap();

        let locator_vector_serialized = serde_json::to_string(&vec![locator.clone()]).unwrap();
        let locators_serialized = serde_json::to_string(&Locators::from(locator.clone())).unwrap();

        assert_eq!(locator_vector_serialized, locators_serialized);

        let locator_vectors_deserialized_as_locators =
            serde_json::from_str::<Locators>(&locator_vector_serialized).unwrap();
        assert_eq!(
            locator_vectors_deserialized_as_locators,
            Locators::from(locator)
        );
    }

    #[test]
    fn empty_locators_fail_to_deserialize() {
        let empty_locators = Vec::<Locator>::new();
        let serialized = serde_json::to_string(&empty_locators).unwrap();
        assert_eq!(
            serde_json::from_str::<Locators>(&serialized)
                .unwrap_err()
                .to_string(),
            "Input cannot be empty."
        );
    }

    #[test]
    fn declaration_initialization() {
        let msg = DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            locators: vec!["/ip4/127.0.0.1/udp/3001/quic-v1".parse().unwrap()]
                .try_into()
                .unwrap(),
            provider_id: Ed25519Key::from_bytes(&[0; _]).public_key().into(),
            zk_id: ZkPublicKey::zero(),
            locked_note_id: Fr::ZERO.into(),
        };

        let declaration = Declaration::new(Epoch::new(10), &msg);
        assert_eq!(declaration.service_type, msg.service_type);
        assert_eq!(declaration.provider_id, msg.provider_id);
        assert_eq!(declaration.locked_note_id, msg.locked_note_id);
        assert_eq!(declaration.locators, msg.locators);
        assert_eq!(declaration.zk_id, msg.zk_id);
        assert_eq!(declaration.created, Epoch::new(10));
        assert_eq!(declaration.active, Epoch::new(12)); // created + SNAPSHOT_FINALIZATION_DELAY
        assert_eq!(declaration.withdraw_at, None);
        assert_eq!(declaration.nonce, 0);
    }

    // Leaf commitment
    fn leaf_declaration() -> Declaration {
        Declaration {
            service_type: ServiceType::BlendNetwork,
            provider_id: Ed25519Key::from_bytes(&[1; _]).public_key().into(),
            locked_note_id: Fr::from(2u64).into(),
            locators: vec!["/ip4/1.1.1.1/udp/3000/quic-v1".parse().unwrap()]
                .try_into()
                .unwrap(),
            zk_id: ZkPublicKey::new(Fr::from(3u64)),
            created: Epoch::new(4),
            active: Epoch::new(5),
            withdraw_at: Some(Epoch::new(6)),
            nonce: 7,
        }
    }

    // The encoding is spelled out field by field, so any change to `leaf_hash`
    // has to be mirrored here and in the specification.
    #[test]
    fn declaration_leaf_matches_the_specified_encoding() {
        let declaration_id = DeclarationId([0u8; 32]);
        let declaration = leaf_declaration();

        let mut info = Vec::new();
        info.extend_from_slice(b"DECLARATION_INFO_HASH_V1");
        info.push(0u8); // ServiceType::BlendNetwork
        for locator in &declaration.locators {
            info.extend_from_slice(locator.as_ref());
        }
        info.extend_from_slice(declaration.provider_id.0.as_bytes());
        for number in declaration.zk_id.as_fr().0.0 {
            info.extend_from_slice(&number.to_le_bytes());
        }
        info.extend_from_slice(&declaration.locked_note_id.as_bytes());
        info.extend_from_slice(&4u32.to_le_bytes());
        info.extend_from_slice(&5u32.to_le_bytes());
        info.extend_from_slice(&6u32.to_le_bytes());
        info.extend_from_slice(&7u64.to_le_bytes());
        let info_hash: Hash = Hasher::digest(&info).into();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&declaration_id.0);
        bytes.extend_from_slice(&info_hash);

        let expected: Hash = Hasher::digest(&bytes).into();
        assert_eq!(declaration.leaf_hash(&declaration_id), expected);
    }

    // A `withdraw_at` that is not set is encoded as zero, like a withdrawal
    // scheduled for epoch 0.
    #[test]
    fn declaration_leaf_encodes_an_unset_withdrawal_as_zero() {
        let declaration_id = DeclarationId([0u8; 32]);
        let unset = Declaration {
            withdraw_at: None,
            ..leaf_declaration()
        };
        let epoch_zero = Declaration {
            withdraw_at: Some(Epoch::new(0)),
            ..leaf_declaration()
        };

        assert_eq!(
            unset.leaf_hash(&declaration_id),
            epoch_zero.leaf_hash(&declaration_id)
        );
    }

    #[test]
    fn declaration_leaf_binds_the_declaration_id() {
        let declaration = leaf_declaration();

        assert_ne!(
            declaration.leaf_hash(&DeclarationId([0u8; 32])),
            declaration.leaf_hash(&DeclarationId([1u8; 32]))
        );
    }

    #[test]
    fn declaration_leaf_binds_every_field() {
        let declaration_id = DeclarationId([0u8; 32]);
        let declaration = leaf_declaration();
        let leaf = declaration.leaf_hash(&declaration_id);

        let mutations = [
            Declaration {
                locators: vec!["/ip4/2.2.2.2/udp/3000/quic-v1".parse().unwrap()]
                    .try_into()
                    .unwrap(),
                ..declaration.clone()
            },
            Declaration {
                provider_id: Ed25519Key::from_bytes(&[9; _]).public_key().into(),
                ..declaration.clone()
            },
            Declaration {
                zk_id: ZkPublicKey::new(Fr::from(9u64)),
                ..declaration.clone()
            },
            Declaration {
                locked_note_id: Fr::from(9u64).into(),
                ..declaration.clone()
            },
            Declaration {
                created: Epoch::new(9),
                ..declaration.clone()
            },
            Declaration {
                active: Epoch::new(9),
                ..declaration.clone()
            },
            Declaration {
                withdraw_at: None,
                ..declaration.clone()
            },
            Declaration {
                nonce: 9,
                ..declaration
            },
        ];

        for mutated in mutations {
            assert_ne!(mutated.leaf_hash(&declaration_id), leaf);
        }
    }

    #[test]
    fn declarations_root_tracks_insertions_and_updates() {
        let declaration_id = DeclarationId([0u8; 32]);
        let declarations = ServiceDeclarations::new();
        let empty_root = declarations.root();

        let (declarations, _) = declarations.insert(declaration_id, leaf_declaration());
        let root = declarations.root();
        assert_ne!(root, empty_root);

        let declarations = declarations
            .update(
                &declaration_id,
                Declaration {
                    nonce: 8,
                    ..leaf_declaration()
                },
            )
            .unwrap();
        assert_ne!(declarations.root(), root);

        // The declaration is updated in place, so restoring it restores the root
        // instead of committing to a second leaf.
        let declarations = declarations
            .update(&declaration_id, leaf_declaration())
            .unwrap();
        assert_eq!(declarations.root(), root);
    }

    // This is why the epoch snapshot drops the inactive declarations from a
    // clone of the live tree instead of rebuilding one from the survivors.
    #[test]
    fn removing_a_declaration_leaves_the_others_in_place() {
        let first = DeclarationId([0u8; 32]);
        let second = DeclarationId([1u8; 32]);

        let (declarations, _) = ServiceDeclarations::new().insert(first, leaf_declaration());
        let (declarations, _) = declarations.insert(second, leaf_declaration());
        let (declarations, _) = declarations.remove(&first).unwrap();

        let (rebuilt, _) = ServiceDeclarations::new().insert(second, leaf_declaration());

        assert_ne!(declarations.root(), rebuilt.root());
    }
}
