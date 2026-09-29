pub mod blend;
pub mod service_notes;

use core::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};
use std::{collections::HashMap, hash::Hash};

use blake2::{Blake2b, Digest as _};
use bytes::Bytes;
use lb_binary_codec::{
    bincode::{self, BoundedSerializeOp, DeserializeOp as _, SerializeOp as _},
    canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError},
};
#[cfg(any(test, feature = "test-utils"))]
use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
    selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
};
use lb_cryptarchia_engine::Epoch;
#[cfg(any(test, feature = "test-utils"))]
use lb_groth16::Fr;
use lb_groth16::fr_to_bytes;
#[cfg(any(test, feature = "test-utils"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, Ed25519Signature, ZkPublicKey};
use lb_utils::bounded::{BoundedVec, NonEmptyBoundedVec};
use multiaddr::{Multiaddr, Protocol};
use serde::{Deserialize, Serialize};
use strum::EnumIter;

use crate::{
    block::BlockNumber,
    mantle::{NoteId, ops::sdp::SdpError, transactions::hash::TxHashView},
    sdp::blend::ActivityProof,
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

type BoundedMultiaddrBytes = BoundedVec<u8, 0, MAX_LOCATOR_BYTE_SIZE>;
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

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for Locator {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .max_length(Some(MAX_LOCATOR_BYTE_SIZE))
            .description(Some(
                "Multiaddress, e.g. `/ip4/127.0.0.1/tcp/3000`.".to_owned(),
            ))
            .examples(["/ip4/127.0.0.1/tcp/3000"])
            .build()
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for Locator {}

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

impl BinaryEncode for Locator {
    fn encoded_length(&self) -> usize {
        self.0.to_vec().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.to_vec().encode_into(out);
    }
}

impl BinaryDecode for Locator {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, value) = BoundedMultiaddrBytes::decode(input, &())?;
        let locator = Self::try_from(value)
            .map_err(|_| DecodeError::invalid_value::<Self>("Invalid locator bytes"))?;
        Ok((rest, locator))
    }
}

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, EnumIter,
)]
pub enum ServiceType {
    #[serde(rename = "BN")]
    BlendNetwork,
    #[cfg(any(test, feature = "test-utils"))]
    #[serde(rename = "TEST")]
    Test,
}

impl AsRef<str> for ServiceType {
    fn as_ref(&self) -> &str {
        match self {
            Self::BlendNetwork => "BN",
            #[cfg(any(test, feature = "test-utils"))]
            Self::Test => "TEST",
        }
    }
}

impl TryFrom<u8> for ServiceType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::BlendNetwork),
            #[cfg(any(test, feature = "test-utils"))]
            1 => Ok(Self::Test),
            _ => Err(()),
        }
    }
}

impl AsRef<u8> for ServiceType {
    fn as_ref(&self) -> &u8 {
        match self {
            Self::BlendNetwork => &0,
            #[cfg(any(test, feature = "test-utils"))]
            Self::Test => &1,
        }
    }
}

impl BinaryEncode for ServiceType {
    fn encoded_length(&self) -> usize {
        <Self as AsRef<u8>>::as_ref(self).encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        <Self as AsRef<u8>>::as_ref(self).encode_into(out);
    }
}

impl BinaryDecode for ServiceType {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, value) = u8::decode(input, &())?;
        let service = Self::try_from(value)
            .map_err(|()| DecodeError::invalid_value::<Self>("unknown service type"))?;
        Ok((rest, service))
    }
}

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

/// The nonce committed to by an SDP operation.
///
/// The wire value is still one little-endian `u64`: the declaration creation
/// epoch occupies the high 32 bits and the sequence occupies the low 32 bits.
/// Since the nonce is encoded in the operation payload, it participates in the
/// Mantle transaction hash covered by the operation's ZK authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
#[serde(transparent)]
pub struct Nonce(u64);

impl Nonce {
    const SEQUENCE_BITS: u32 = 32;

    #[must_use]
    pub const fn new(lifecycle_epoch: Epoch, sequence: u32) -> Self {
        Self(((lifecycle_epoch.into_inner() as u64) << Self::SEQUENCE_BITS) | sequence as u64)
    }

    #[must_use]
    pub const fn lifecycle_epoch(self) -> Epoch {
        Epoch::new((self.0 >> Self::SEQUENCE_BITS) as u32)
    }

    #[must_use]
    pub const fn sequence(self) -> u32 {
        self.0 as u32
    }

    /// Return the next sequence in this lifecycle, without wrapping into the
    /// lifecycle epoch bits.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        let sequence = self.sequence();
        if sequence == u32::MAX {
            None
        } else {
            Some(Self::new(self.lifecycle_epoch(), sequence + 1))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ProviderId(pub Ed25519PublicKey);

impl AsRef<[u8; 32]> for ProviderId {
    fn as_ref(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

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
        self.as_ref().cmp(other.as_ref())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, BinaryCodec)]
pub struct DeclarationId(pub [u8; 32]);
serde_bytes_newtype!(DeclarationId, 32);
display_hex_bytes_newtype!(DeclarationId);

impl AsRef<[u8; 32]> for DeclarationId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl BoundedSerializeOp for DeclarationId {
    type Bytes = [u8; 32];
}

fn declaration_id_preimage(service_type: ServiceType, zk_id: ZkPublicKey) -> [u8; 33] {
    let mut preimage = [0; 33];
    preimage[0] = *service_type.as_ref();
    preimage[1..].copy_from_slice(&fr_to_bytes(zk_id.as_fr()));
    preimage
}

fn declaration_id(service_type: ServiceType, zk_id: ZkPublicKey) -> DeclarationId {
    let mut hasher = Blake2b::new();
    hasher.update(declaration_id_preimage(service_type, zk_id));

    DeclarationId(hasher.finalize().into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub service_type: ServiceType,
    pub provider_id: ProviderId,
    pub service_note_id: NoteId,
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
    pub fn id(&self) -> DeclarationId {
        declaration_id(self.service_type, self.zk_id)
    }

    #[must_use]
    pub fn new(epoch: Epoch, declaration_msg: &DeclarationMessage) -> Self {
        Self {
            service_type: declaration_msg.service_type,
            provider_id: declaration_msg.provider_id,
            service_note_id: declaration_msg.service_note_id,
            locators: declaration_msg.locators.clone(),
            zk_id: declaration_msg.zk_id,
            created: epoch,
            active: epoch.strict_add(SNAPSHOT_FINALIZATION_DELAY),
            withdraw_at: None,
            nonce: Nonce::new(epoch, 0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Declarations(HashMap<ServiceType, HashMap<DeclarationId, Declaration>>);

impl Declarations {
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (&ServiceType, &HashMap<DeclarationId, Declaration>)> {
        self.0.iter()
    }

    #[must_use]
    pub fn for_service(
        &self,
        service_type: &ServiceType,
    ) -> Option<&HashMap<DeclarationId, Declaration>> {
        self.0.get(service_type)
    }
}

impl From<HashMap<ServiceType, HashMap<DeclarationId, Declaration>>> for Declarations {
    fn from(value: HashMap<ServiceType, HashMap<DeclarationId, Declaration>>) -> Self {
        Self(value)
    }
}

impl FromIterator<(ServiceType, HashMap<DeclarationId, Declaration>)> for Declarations {
    fn from_iter<I: IntoIterator<Item = (ServiceType, HashMap<DeclarationId, Declaration>)>>(
        iter: I,
    ) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl TryFrom<Bytes> for Declarations {
    type Error = bincode::Error;

    fn try_from(bytes: Bytes) -> Result<Self, Self::Error> {
        Self::from_bytes(&bytes)
    }
}

impl TryFrom<Declarations> for Bytes {
    type Error = bincode::Error;

    fn try_from(this: Declarations) -> Result<Self, Self::Error> {
        this.to_bytes()
    }
}

pub const MAX_DECLARATION_LOCATOR_COUNT: usize = 8;
pub type Locators = NonEmptyBoundedVec<Locator, MAX_DECLARATION_LOCATOR_COUNT>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct DeclarationMessage {
    pub service_type: ServiceType,
    pub locators: Locators,
    pub provider_id: ProviderId,
    pub zk_id: ZkPublicKey,
    pub service_note_id: NoteId,
}

impl DeclarationMessage {
    #[must_use]
    pub fn id(&self) -> DeclarationId {
        declaration_id(self.service_type, self.zk_id)
    }

    pub(crate) fn preverify(
        &self,
        tx_hash_view: &TxHashView,
        proof_eddsa_signature: &Ed25519Signature,
    ) -> Result<(), SdpError> {
        // Ensure ownership over the `provider_id`
        self.provider_id
            .0
            .verify(tx_hash_view.as_bytes(), proof_eddsa_signature)
            .map_err(|_| SdpError::InvalidEddsaSignature)?;

        Ok(())
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            service_type: ServiceType::BlendNetwork,
            locators: "/ip4/127.0.0.1/udp/3000/quic-v1"
                .parse::<Locator>()
                .expect("Locator is valid.")
                .into(),
            provider_id: ProviderId(Ed25519Key::from_bytes(&[24; 32]).public_key()),
            zk_id: ZkPublicKey::from(Fr::from(25u64)),
            service_note_id: NoteId(Fr::from(26u64)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct WithdrawMessage {
    pub declaration_id: DeclarationId,
    pub nonce: Nonce,
}

// ActiveMessage = DeclarationId Nonce Metadata — plain field-order concat.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ActiveMessage {
    pub declaration_id: DeclarationId,
    pub nonce: Nonce,
    pub metadata: ActivityMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ActivityMetadata {
    Blend(Box<ActivityProof>),
}

impl ActivityMetadata {
    /// Returns the epoch whose activity is proven by this metadata.
    #[must_use]
    pub const fn origin_epoch(&self) -> Epoch {
        match self {
            Self::Blend(proof) => proof.epoch,
        }
    }

    /// The epoch during which this activity must be submitted/accepted.
    #[must_use]
    pub fn submission_epoch(&self) -> Epoch {
        match self {
            Self::Blend(proof) => proof.epoch.strict_add(1.into()),
        }
    }
}

const ACTIVE_METADATA_BLEND_TYPE: u8 = 1;

impl BinaryEncode for ActivityMetadata {
    fn encoded_length(&self) -> usize {
        match self {
            Self::Blend(proof) => {
                ACTIVE_METADATA_BLEND_TYPE.encoded_length() + proof.encoded_length()
            }
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Blend(proof) => {
                ACTIVE_METADATA_BLEND_TYPE.encode_into(out);
                proof.encode_into(out);
            }
        }
    }
}

impl BinaryDecode for ActivityMetadata {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, metadata_type) = u8::decode(input, &())?;
        match metadata_type {
            ACTIVE_METADATA_BLEND_TYPE => {
                let (input, proof) = ActivityProof::decode(input, &())?;
                Ok((input, Self::Blend(Box::new(proof))))
            }
            other => Err(DecodeError::unknown_discriminant::<Self>(u64::from(other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::bincode::{BoundedSerializeOp as _, SerializeOp as _};
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};
    use multiaddr::Multiaddr;

    use super::declaration_id_preimage;
    use crate::sdp::{
        Declaration, DeclarationId, DeclarationMessage, Locator, Locators, Nonce, ProviderId,
        ServiceType,
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
        assert!(
            serde_json::from_str::<Locators>(&serialized)
                .unwrap_err()
                .to_string()
                .contains("Input cannot be empty."),
            "empty locators should be rejected"
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
            service_note_id: Fr::ZERO.into(),
        };

        let declaration = Declaration::new(Epoch::new(10), &msg);
        assert_eq!(declaration.service_type, msg.service_type);
        assert_eq!(declaration.provider_id, msg.provider_id);
        assert_eq!(declaration.service_note_id, msg.service_note_id);
        assert_eq!(declaration.locators, msg.locators);
        assert_eq!(declaration.zk_id, msg.zk_id);
        assert_eq!(declaration.created, Epoch::new(10));
        assert_eq!(declaration.active, Epoch::new(12)); // created + SNAPSHOT_FINALIZATION_DELAY
        assert_eq!(declaration.withdraw_at, None);
        assert_eq!(declaration.nonce, Nonce::new(declaration.created, 0));
        assert_eq!(declaration.nonce.lifecycle_epoch(), declaration.created);
        assert_eq!(declaration.nonce.sequence(), 0);
    }

    fn declaration_message(locators: Vec<Locator>) -> DeclarationMessage {
        DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            locators: locators.try_into().unwrap(),
            provider_id: Ed25519Key::from_bytes(&[1; _]).public_key().into(),
            zk_id: ZkPublicKey::new(Fr::from(3u64)),
            service_note_id: Fr::from(2u64).into(),
        }
    }

    #[test]
    fn declaration_id_matches_rfc_407_vector() {
        let message = DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            zk_id: ZkPublicKey::new(Fr::from(25u64)),
            ..declaration_message(vec!["/ip4/203.0.113.10/tcp/4001".parse().unwrap()])
        };

        let preimage = declaration_id_preimage(message.service_type, message.zk_id);
        let mut expected_preimage = [0; 33];
        expected_preimage[1] = 0x19;
        assert_eq!(preimage.len(), 33);
        assert_eq!(preimage, expected_preimage);

        assert_eq!(
            message.id().0,
            [
                0x67, 0xfa, 0x7d, 0x1f, 0xe7, 0xf1, 0x39, 0x11, 0x95, 0xfd, 0xd4, 0x79, 0xbb, 0xa3,
                0x0d, 0xd9, 0x7c, 0x9e, 0x6d, 0x2e, 0x15, 0x25, 0x43, 0x6a, 0xc2, 0x42, 0x7f, 0x2b,
                0xf8, 0x56, 0x7a, 0x0d,
            ]
        );
        assert_eq!(Declaration::new(Epoch::new(0), &message).id(), message.id());
    }

    /// Declaration identity depends only on its service and `zk_id`.
    #[test]
    fn declaration_id_ignores_provider_locators_and_service_note() {
        let original = declaration_message(vec!["/ip4/203.0.113.10/tcp/4001".parse().unwrap()]);
        let different_provider = DeclarationMessage {
            provider_id: Ed25519Key::from_bytes(&[2; 32]).public_key().into(),
            ..original.clone()
        };
        let different_locators =
            declaration_message(vec!["/ip4/203.0.113.11/tcp/4002".parse().unwrap()]);
        let different_note = DeclarationMessage {
            service_note_id: Fr::from(99u64).into(),
            ..original.clone()
        };

        assert_eq!(original.id(), different_provider.id());
        assert_eq!(original.id(), different_locators.id());
        assert_eq!(original.id(), different_note.id());
    }

    /// Reusing a `zk_id` under another service produces a distinct identity.
    #[cfg(any(test, feature = "test-utils"))]
    #[test]
    fn declaration_id_is_service_scoped() {
        let blend = declaration_message(vec!["/ip4/203.0.113.10/tcp/4001".parse().unwrap()]);
        let other_service = DeclarationMessage {
            service_type: ServiceType::Test,
            ..blend.clone()
        };

        assert_eq!(blend.zk_id, other_service.zk_id);
        assert_ne!(blend.id(), other_service.id());
    }

    #[test]
    fn declaration_id_has_exact_bincode_size() {
        let id = DeclarationId([0x66; 32]);
        let ordinary = id.to_bytes().unwrap();
        let bounded = id.to_bounded_bytes().unwrap();

        assert_eq!(ordinary.len(), 32);
        assert_eq!(bounded.as_ref(), ordinary.as_ref());
    }

    #[test]
    fn sdp_byte_types_borrow_their_stored_bytes() {
        let provider_id = ProviderId(Ed25519Key::from_bytes(&[0; 32]).public_key());
        let declaration_id = DeclarationId([0x66; 32]);

        assert_eq!(provider_id.as_ref(), provider_id.0.as_bytes());
        assert_eq!(declaration_id.as_ref(), &declaration_id.0);
        assert!(std::ptr::eq(provider_id.as_ref(), provider_id.0.as_bytes()));
        assert!(std::ptr::eq(
            declaration_id.as_ref(),
            &raw const declaration_id.0
        ));
    }
}

#[cfg(test)]
mod nonce_tests {
    use lb_binary_codec::canonical::{BinaryDecodeExt as _, BinaryEncode as _};
    use lb_cryptarchia_engine::Epoch;

    use crate::sdp::Nonce;

    #[test]
    fn pack_unpack_roundtrips_boundary_values() {
        for (epoch, sequence) in [(0, 0), (0, 1), (1, 0), (u32::MAX, u32::MAX)] {
            let nonce = Nonce::new(Epoch::new(epoch), sequence);
            assert_eq!(nonce.lifecycle_epoch(), Epoch::new(epoch));
            assert_eq!(nonce.sequence(), sequence);
        }
    }

    #[test]
    fn canonical_encoding_is_the_same_little_endian_u64() {
        let nonce = Nonce::new(Epoch::new(0x0102_0304), 0x0506_0708);

        assert_eq!(nonce.encoded_length(), 8);
        assert_eq!(nonce.encode_to_vec(), [8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(Nonce::decode_all(&nonce.encode_to_vec()).unwrap(), nonce);
    }

    #[test]
    fn serde_keeps_nonce_as_a_scalar_integer() {
        let nonce = Nonce::new(Epoch::new(1), 2);
        let json = serde_json::to_string(&nonce).unwrap();

        assert_eq!(json, "4294967298");
        assert_eq!(serde_json::from_str::<Nonce>(&json).unwrap(), nonce);
    }

    #[test]
    fn checked_next_does_not_wrap_sequence_overflow() {
        let last = Nonce::new(Epoch::new(10), u32::MAX);
        assert_eq!(last.checked_next(), None);

        let next = Nonce::new(Epoch::new(10), 4).checked_next().unwrap();
        assert_eq!(next.lifecycle_epoch(), Epoch::new(10));
        assert_eq!(next.sequence(), 5);
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl WithdrawMessage {
    #[must_use]
    pub const fn sample() -> Self {
        Self {
            declaration_id: DeclarationId([27u8; 32]),
            nonce: Nonce::new(Epoch::new(0), 29),
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl ActiveMessage {
    #[must_use]
    pub fn sample() -> Self {
        let activity = ActivityProof {
            epoch: Epoch::new(10),
            signing_key: Ed25519Key::from_bytes(&[1; 32]).public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([2u8; PROOF_OF_QUOTA_SIZE])
                .into(),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked(
                [3u8; PROOF_OF_SELECTION_SIZE],
            )
            .into(),
        };

        Self {
            declaration_id: DeclarationId([30u8; 32]),
            nonce: Nonce::new(Epoch::new(0), 31),
            metadata: ActivityMetadata::Blend(Box::new(activity)),
        }
    }
}
