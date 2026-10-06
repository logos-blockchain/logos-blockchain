mod deser;
mod fixtures;
pub mod genesis;

pub mod v1;

use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::{
    Slot, UncleSlots,
    era::{EraSchedule, EraVersion},
};
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};
use lb_utils::bounded::{BoundedError, BoundedVec, UpperBoundedOrderedSet, UpperBoundedVec};
use serde::{Deserialize, Serialize};

use crate::{
    era::EraSchedules,
    header::{HeaderId, HeaderRef},
    mantle::{
        traits::{Hashable, StorageSize},
        transactions::hash::{TxHash, TxHashPrefix},
    },
    proofs::leader_proof::Groth16LeaderProof,
};

/// The maximum number of transactions allowed in a block.
const MAX_BLOCK_TRANSACTIONS: usize = 1024;
/// The maximum total size of all transactions in a block, in bytes (2 MiB).
/// Note: This is not the total block size.
pub const MAX_BLOCK_TRANSACTIONS_SIZE: usize = 1024 * 1024 * 2;

pub type BlockNumber = u64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to serialize: {0}")]
    Serialisation(#[from] lb_binary_codec::bincode::Error),
    #[error("Invalid block signature")]
    Signature,
    #[error("Failed to verify header alone: {0}")]
    Header(#[from] HeaderError),
    #[error("Body root mismatch: calculated body does not match header")]
    BodyRootMismatch,
    #[error("Signing key does not match the leader key in proof of leadership")]
    KeyMismatch,
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
    #[error("Total storage size {size} exceeds maximum of {max} bytes")]
    ContentTooBig { size: usize, max: usize },
}

/// Why a header fails the checks that need the header alone.
#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("Expected a non-genesis slot")]
    GenesisSlot,
}

/// Transaction-hash prefixes referenced by a block proposal.
pub type BlockTransactionReferences = UpperBoundedVec<TxHashPrefix, MAX_BLOCK_TRANSACTIONS>;

/// References to transactions that are included in a block proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BinaryCodec)]
pub struct References {
    /// Bounded hashes of the transactions that are included in the block
    /// proposal.
    pub mempool_transactions: BlockTransactionReferences,
}

impl References {
    /// Maximum canonical representation of the bounded transaction references.
    pub const MAX_CANONICAL_ENCODED_SIZE: usize =
        2 + BlockTransactionReferences::MAX * TxHashPrefix::CANONICAL_ENCODED_SIZE;

    /// Constructs a `References` instance from a list of transactions,
    /// extracting their hashes.
    #[must_use]
    pub(crate) fn from_block_transactions<Tx>(transactions: &BlockTransactions<Tx>) -> Self
    where
        Tx: Hashable<Hash = TxHash>,
    {
        Self {
            mempool_transactions: transactions
                .map_ref(|transaction| Tx::hash(transaction).prefix()),
        }
    }
}

/// Validated transaction payload for blocks.
///
/// The block stores transactions as this bounded vector directly, so the
/// transaction-count limit is enforced at construction and deserialization
/// boundaries.
pub type BlockTransactions<Tx> = BoundedVec<Tx, 0, MAX_BLOCK_TRANSACTIONS>;

/// A block proposal, of the version of the era of its slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proposal {
    V1(v1::Proposal),
}

impl Proposal {
    /// The largest canonical encoding of a proposal of any version.
    pub const MAX_ENCODED_SIZE: usize = v1::Proposal::MAX_ENCODED_SIZE;

    #[must_use]
    pub const fn header(&self) -> HeaderRef<'_> {
        match self {
            Self::V1(proposal) => HeaderRef::V1(proposal.header()),
        }
    }

    #[must_use]
    pub const fn uncle_headers(&self) -> UncleHeadersRef<'_> {
        match self {
            Self::V1(proposal) => UncleHeadersRef::V1(proposal.uncle_headers()),
        }
    }

    #[must_use]
    pub const fn references(&self) -> &References {
        match self {
            Self::V1(proposal) => proposal.references(),
        }
    }

    /// The reference prefixes carried by this proposal, in block order.
    #[must_use]
    pub fn mempool_transactions(&self) -> &[TxHashPrefix] {
        match self {
            Self::V1(proposal) => proposal.mempool_transactions(),
        }
    }

    #[must_use]
    pub const fn signature(&self) -> &Ed25519Signature {
        match self {
            Self::V1(proposal) => proposal.signature(),
        }
    }
}

impl BinaryEncode for Proposal {
    fn encoded_length(&self) -> usize {
        match self {
            Self::V1(proposal) => proposal.encoded_length(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::V1(proposal) => proposal.encode_into(out),
        }
    }
}

/// Decodes a proposal with the codec of the version of the era of its slot,
/// read off the start of its encoding.
impl BinaryDecode for Proposal {
    type Context = EraSchedules;

    fn decode<'input>(
        input: &'input [u8],
        eras: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        // The slot opens the proposal's header, which reads it again.
        let slot = Slot::peek_decode(input, &())?;
        let era_for_slot = eras.at_slot(slot).entry.version;
        match era_for_slot {
            EraVersion::V1 => {
                v1::Proposal::decode(input, &()).map(|(rest, proposal)| (rest, Self::V1(proposal)))
            }
        }
    }
}

/// A block, of the version of the era of its slot.
///
/// Its canonical encoding is its version's, which the era of its slot picks.
/// Its serde form is tagged with its version instead, so that it reads back
/// without the era schedule: deserializing checks the block as its version's
/// `reconstruct` does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Tx: Clone + Serialize",
    deserialize = "Tx: Clone + Eq + Deserialize<'de> + Hashable<Hash = TxHash> + StorageSize"
))]
pub enum Block<Tx> {
    V1(v1::Block<Tx>),
}

impl<Tx> Block<Tx> {
    /// The largest canonical encoding of a block of any version whose
    /// transactions take as many bytes to encode as their storage size counts.
    pub const MAX_ENCODED_SIZE: usize = v1::Block::<Tx>::MAX_ENCODED_SIZE;

    /// Builds and signs a block of the version of its uncle headers, which a
    /// leader gathers for the era of `slot`.
    pub fn create(
        parent_block: HeaderId,
        slot: Slot,
        uncle_headers: UncleHeaders,
        proof_of_leadership: Groth16LeaderProof,
        transactions: BlockTransactions<Tx>,
        signing_key: &Ed25519Key,
    ) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        match uncle_headers {
            UncleHeaders::V1(uncle_headers) => v1::Block::create(
                parent_block,
                slot,
                uncle_headers,
                proof_of_leadership,
                transactions,
                signing_key,
            )
            .map(Self::V1),
        }
    }

    /// The block `proposal` proposes, given the transactions its references
    /// name, checked as a block decoded from bytes is.
    pub fn from_proposal(
        proposal: Proposal,
        transactions: BlockTransactions<Tx>,
    ) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        match proposal {
            Proposal::V1(v1::Proposal {
                header,
                uncle_headers,
                signature,
                ..
            }) => {
                v1::Block::reconstruct(header, uncle_headers, transactions, signature).map(Self::V1)
            }
        }
    }

    #[must_use]
    pub const fn header(&self) -> HeaderRef<'_> {
        match self {
            Self::V1(block) => HeaderRef::V1(block.header()),
        }
    }

    #[must_use]
    pub const fn uncle_headers(&self) -> UncleHeadersRef<'_> {
        match self {
            Self::V1(block) => UncleHeadersRef::V1(block.uncle_headers()),
        }
    }

    pub fn transactions_iter(&self) -> impl ExactSizeIterator<Item = &Tx> + '_ {
        match self {
            Self::V1(block) => block.transactions_iter(),
        }
    }

    #[must_use]
    pub const fn transactions(&self) -> &BlockTransactions<Tx> {
        match self {
            Self::V1(block) => block.transactions(),
        }
    }

    #[must_use]
    pub fn into_transactions(self) -> Vec<Tx> {
        match self {
            Self::V1(block) => block.into_transactions(),
        }
    }

    #[must_use]
    pub const fn signature(&self) -> &Ed25519Signature {
        match self {
            Self::V1(block) => block.signature(),
        }
    }

    #[must_use]
    pub fn to_proposal(self) -> Proposal
    where
        Tx: Hashable<Hash = TxHash>,
    {
        match self {
            Self::V1(block) => Proposal::V1(block.to_proposal()),
        }
    }
}

impl<Tx> BinaryEncode for Block<Tx>
where
    Tx: BinaryEncode + Hashable<Hash = TxHash> + StorageSize,
{
    fn encoded_length(&self) -> usize {
        match self {
            Self::V1(block) => block.encoded_length(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::V1(block) => block.encode_into(out),
        }
    }
}

/// Decodes a block with the codec of the version of the era of its slot,
/// read off the start of its encoding, and checks it.
impl<Tx> BinaryDecode for Block<Tx>
where
    Tx: BinaryDecode + Hashable<Hash = TxHash> + StorageSize,
{
    type Context = (EraSchedule<()>, Tx::Context);

    fn decode<'input>(
        input: &'input [u8],
        (eras, tx_decode_context): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        // The slot opens the block's header, which reads it again.
        let slot = Slot::peek_decode(input, &())?;
        let era_for_slot = eras.at_slot(slot).entry.version;
        match era_for_slot {
            EraVersion::V1 => <v1::Block<Tx>>::decode(input, tx_decode_context)
                .map(|(rest, block)| (rest, Self::V1(block))),
        }
    }
}

/// The uncle headers a leader gathers for a new block, of the version of the
/// era of the block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UncleHeaders {
    V1(v1::UncleHeaders),
}

impl UncleHeaders {
    /// No uncle headers, for a block of an era of `version`.
    #[must_use]
    pub fn empty(version: EraVersion) -> Self {
        match version {
            EraVersion::V1 => Self::V1(v1::UncleHeaders::empty()),
        }
    }

    /// The signed headers of `blocks`, as the uncles of a block of an era of
    /// `version`. An uncle is of the era of the block that carries it, and so
    /// of its version.
    pub fn of_blocks<'block, Tx: 'block>(
        version: EraVersion,
        blocks: impl IntoIterator<Item = &'block Block<Tx>>,
    ) -> Result<Self, BoundedError> {
        match version {
            EraVersion::V1 => {
                let signed_headers = blocks.into_iter().map(|block| match block {
                    Block::V1(block) => block.signed_header(),
                });
                Ok(Self::V1(v1::UncleHeaders::new(
                    UpperBoundedOrderedSet::try_from_iter(signed_headers)?,
                )))
            }
        }
    }

    /// The slots of the uncles.
    #[must_use]
    pub fn slots(&self) -> UncleSlots {
        match self {
            Self::V1(uncle_headers) => uncle_headers.slots(),
        }
    }
}

/// The uncle headers of a block or a proposal of any version, borrowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UncleHeadersRef<'block> {
    V1(&'block v1::UncleHeaders),
}

impl<'block> UncleHeadersRef<'block> {
    /// The slots of the uncles.
    #[must_use]
    pub fn slots(self) -> UncleSlots {
        match self {
            Self::V1(uncle_headers) => uncle_headers.slots(),
        }
    }

    pub fn ids(self) -> impl Iterator<Item = HeaderId> + 'block {
        match self {
            Self::V1(uncle_headers) => uncle_headers.ids(),
        }
    }

    pub fn parents(self) -> impl Iterator<Item = HeaderId> + 'block {
        match self {
            Self::V1(uncle_headers) => uncle_headers.parents(),
        }
    }

    #[must_use]
    pub fn len(self) -> usize {
        match self {
            Self::V1(uncle_headers) => uncle_headers.len(),
        }
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        match self {
            Self::V1(uncle_headers) => uncle_headers.is_empty(),
        }
    }

    pub fn iter(self) -> impl Iterator<Item = SignedHeaderRef<'block>> {
        match self {
            Self::V1(uncle_headers) => uncle_headers.iter().map(SignedHeaderRef::V1),
        }
    }
}

/// An uncle header of any version and the signature of its leader, borrowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedHeaderRef<'block> {
    V1(&'block v1::SignedHeader),
}

impl<'block> SignedHeaderRef<'block> {
    #[must_use]
    pub const fn header(self) -> HeaderRef<'block> {
        match self {
            Self::V1(signed_header) => HeaderRef::V1(signed_header.header()),
        }
    }

    #[must_use]
    pub const fn signature(self) -> &'block Ed25519Signature {
        match self {
            Self::V1(signed_header) => signed_header.signature(),
        }
    }

    /// Checks the header alone and the signature of its leader.
    pub fn verify(self) -> Result<(), Error> {
        match self {
            Self::V1(signed_header) => signed_header.verify(),
        }
    }
}

/// Validates the header using only the content within the header.
///
/// This does not check `body_root` because it commits to a body this function
/// does not have. It should be checked separately by the caller.
///
/// This does not check `proof_of_leadership` and the parent header
/// since they require a ledger state.
pub fn verify_header_alone(header: HeaderRef<'_>) -> Result<(), HeaderError> {
    match header {
        HeaderRef::V1(header) => v1::verify_header_alone(header),
    }
}

/// Verifies the signature of block header.
pub fn verify_header_signature(
    header: HeaderRef<'_>,
    signature: &Ed25519Signature,
) -> Result<(), Error> {
    match header {
        HeaderRef::V1(header) => v1::verify_header_signature(header, signature),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{block::fixtures::single_era, mantle::transactions::Ops};

    fn block() -> Block<Ops> {
        Block::create(
            [0u8; 32].into(),
            Slot::from(0x0102_0304_0506_0708u64),
            UncleHeaders::empty(EraVersion::V1),
            v1::tests::create_proof(),
            BlockTransactions::try_from(vec![Ops::new_unchecked(vec![]); 3]).unwrap(),
            &Ed25519Key::from_bytes(&[0; 32]),
        )
        .expect("valid block")
    }

    /// A block decodes under the version of the era of the slot its
    /// encoding starts with.
    #[test]
    fn a_block_decodes_under_the_era_of_its_slot() {
        let block = block();

        assert_eq!(
            Block::decode_all(&block.encode(), &(single_era(), ())).unwrap(),
            block
        );
    }

    /// A block's serde form is tagged with its version, so it reads back
    /// without the era schedule, as storage reads it.
    #[test]
    fn a_block_reads_back_from_its_version_tagged_serde_form() {
        use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};

        let block = block();

        let json = serde_json::to_value(&block).unwrap();
        assert!(json.get("V1").is_some());
        assert_eq!(serde_json::from_value::<Block<Ops>>(json).unwrap(), block);
        assert_eq!(
            Block::<Ops>::from_bytes(&block.to_bytes().unwrap()).unwrap(),
            block
        );
    }
}
