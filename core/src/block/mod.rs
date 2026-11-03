mod deser;
mod fixtures;
pub mod genesis;
mod uncle;

use core::fmt::Debug;

use lb_binary_codec::canonical::{
    BinaryCodec, BinaryDecode, BinaryEncode, DecodeError, length_prefix_len,
};
use lb_cryptarchia_engine::Slot;
use lb_utils::bounded::{BoundedError, BoundedVec, UpperBoundedVec};
use serde::{Deserialize, Serialize};
pub use uncle::UncleHeaders;

use crate::{
    crypto::{Digest as _, Hasher},
    header::{ContentId, Header, HeaderId},
    mantle::{
        traits::{Hashable, StorageSize},
        transactions::hash::{TxHash, TxHashPrefix},
    },
    proofs::leader_proof::Groth16LeaderProof,
    utils::merkle,
};

/// The maximum number of transactions allowed in a block.
const MAX_BLOCK_TRANSACTIONS: usize = 1024;
/// The bytes a block's transactions, or a proposal's references to them, are
/// counted in when canonically encoded: the length prefix of a collection of
/// at most [`MAX_BLOCK_TRANSACTIONS`] items.
const BLOCK_TRANSACTIONS_COUNT_SIZE: usize = length_prefix_len::<MAX_BLOCK_TRANSACTIONS>();
/// The maximum total size of all transactions in a block, in bytes (2 MiB).
/// Note: This is not the total block size.
pub const MAX_BLOCK_TRANSACTIONS_SIZE: usize = 1024 * 1024 * 2;

pub type BlockNumber = u64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to serialize: {0}")]
    Serialisation(#[from] lb_binary_codec::bincode::Error),
    #[error("Failed to verify header alone: {0}")]
    Header(#[from] HeaderError),
    #[error("Body root mismatch: calculated body does not match header")]
    BodyRootMismatch,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub header: Header,
    pub uncle_headers: UncleHeaders,
    pub references: References,
}

impl BinaryEncode for Proposal {
    fn encoded_length(&self) -> usize {
        let Self {
            header,
            uncle_headers,
            references,
        } = self;

        header.encoded_length() + uncle_headers.encoded_length() + references.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            header,
            uncle_headers,
            references,
        } = self;

        header.encode_into(out);
        uncle_headers.encode_into(out);
        references.encode_into(out);
    }
}

impl BinaryDecode for Proposal {
    /// The most uncles the era of the proposal's slot allows.
    type Context = u8;

    fn decode<'input>(
        input: &'input [u8],
        max_uncles: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, header) = Header::decode(input, &())?;
        let (input, uncle_headers) = UncleHeaders::decode(input, max_uncles)?;
        let (input, references) = References::decode(input, &())?;
        Ok((
            input,
            Self {
                header,
                uncle_headers,
                references,
            },
        ))
    }
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
    pub const MAX_CANONICAL_ENCODED_SIZE: usize = BLOCK_TRANSACTIONS_COUNT_SIZE
        + BlockTransactionReferences::MAX * TxHashPrefix::CANONICAL_ENCODED_SIZE;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(bound(serialize = "Tx: Clone + Serialize"))]
pub struct Block<Tx> {
    header: Header,
    uncle_headers: UncleHeaders,
    transactions: BlockTransactions<Tx>,
}

impl<'de, Tx> Deserialize<'de> for Block<Tx>
where
    Tx: Clone + Eq + Deserialize<'de> + Hashable<Hash = TxHash> + StorageSize,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawBlock<Tx> {
            header: Header,
            uncle_headers: UncleHeaders,
            transactions: BlockTransactions<Tx>,
        }

        let raw = RawBlock::<Tx>::deserialize(deserializer)?;

        // Serde reads blocks a node serves through its API, already checked
        // against their era by that node, so it holds them to no era.
        Self::reconstruct(raw.header, raw.uncle_headers, raw.transactions)
            .map_err(serde::de::Error::custom)
    }
}

impl<Tx> BinaryEncode for Block<Tx>
where
    Tx: BinaryEncode + Hashable<Hash = TxHash> + StorageSize,
{
    fn encoded_length(&self) -> usize {
        let Self {
            header,
            uncle_headers,
            transactions,
        } = self;

        header.encoded_length() + uncle_headers.encoded_length() + transactions.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            header,
            uncle_headers,
            transactions,
        } = self;

        header.encode_into(out);
        uncle_headers.encode_into(out);
        transactions.encode_into(out);
    }
}

// Decoding checks the block as `reconstruct` does.
impl<Tx> BinaryDecode for Block<Tx>
where
    Tx: BinaryDecode + Hashable<Hash = TxHash> + StorageSize,
{
    /// The most uncles the era of the block's slot allows.
    type Context = (u8, Tx::Context);

    fn decode<'input>(
        input: &'input [u8],
        (max_uncles, tx_context): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, header) = Header::decode(input, &())?;
        let (input, uncle_headers) = UncleHeaders::decode(input, max_uncles)?;
        let (input, transactions) = BlockTransactions::<Tx>::decode(input, tx_context)?;
        let block = Self::reconstruct(header, uncle_headers, transactions)
            .map_err(|error| DecodeError::invalid_value::<Self>(error.to_string()))?;
        Ok((input, block))
    }
}

impl Proposal {
    /// Maximum canonical encoded size of a block proposal whose era allows
    /// `max_uncles`.
    ///
    /// The bound is composed from the maxima owned by each component rather
    /// than being an unexplained total. It describes the canonical encoding,
    /// not the configured-bincode representation.
    #[must_use]
    pub fn max_encoded_size(max_uncles: u8) -> usize {
        Header::CANONICAL_ENCODED_SIZE
            + UncleHeaders::max_encoded_size(max_uncles)
            + References::MAX_CANONICAL_ENCODED_SIZE
    }

    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub const fn uncle_headers(&self) -> &UncleHeaders {
        &self.uncle_headers
    }

    #[must_use]
    pub const fn references(&self) -> &References {
        &self.references
    }

    /// The reference prefixes carried by this proposal, in block order.
    #[must_use]
    pub fn mempool_transactions(&self) -> &[TxHashPrefix] {
        &self.references.mempool_transactions
    }
}

/// Validated transaction payload for blocks.
///
/// The block stores transactions as this bounded vector directly, so the
/// transaction-count limit is enforced at construction and deserialization
/// boundaries.
pub type BlockTransactions<Tx> = BoundedVec<Tx, 0, MAX_BLOCK_TRANSACTIONS>;

impl<Tx> Block<Tx> {
    pub fn create(
        parent_block: HeaderId,
        slot: Slot,
        uncle_headers: UncleHeaders,
        proof_of_leadership: Groth16LeaderProof,
        transactions: BlockTransactions<Tx>,
    ) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        // 1. Non-genesis blocks only
        if slot == Slot::genesis() {
            return Err(HeaderError::GenesisSlot.into());
        }

        // 3. Body root & header
        let header = Header::new(
            parent_block,
            body_root(&uncle_headers, transactions.as_slice()),
            slot,
            proof_of_leadership,
        );

        // 5. New block
        let block = Self {
            header,
            uncle_headers,
            transactions,
        };

        // 6. Size is ok
        block.validate_total_transactions_size()?;

        Ok(block)
    }

    pub fn reconstruct(
        header: Header,
        uncle_headers: UncleHeaders,
        transactions: BlockTransactions<Tx>,
    ) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        let block = Self {
            header,
            uncle_headers,
            transactions,
        };
        let block = block.into_verified()?;

        Ok(block)
    }

    fn into_verified(self) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        // 1. Checks that need the header alone
        verify_header_alone(&self.header)?;

        // 2. Size is ok
        self.validate_total_transactions_size()?;

        // 3. Body root matches the carried uncle headers and transactions
        self.validate_body_root()?;

        Ok(self)
    }

    /// Maximum canonical encoded size of a block whose era allows
    /// `max_uncles`: its transactions take at most
    /// [`MAX_BLOCK_TRANSACTIONS_SIZE`] bytes besides their count.
    #[must_use]
    pub fn max_encoded_size(max_uncles: u8) -> usize {
        Header::CANONICAL_ENCODED_SIZE
            + UncleHeaders::max_encoded_size(max_uncles)
            + BLOCK_TRANSACTIONS_COUNT_SIZE
            + MAX_BLOCK_TRANSACTIONS_SIZE
    }

    fn validate_total_transactions_size(&self) -> Result<usize, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        let mut total = 0usize;

        for item in &self.transactions {
            total = total
                .checked_add(item.storage_size())
                .ok_or(Error::ContentTooBig {
                    size: usize::MAX,
                    max: MAX_BLOCK_TRANSACTIONS_SIZE,
                })?;

            if total > MAX_BLOCK_TRANSACTIONS_SIZE {
                return Err(Error::ContentTooBig {
                    size: total,
                    max: MAX_BLOCK_TRANSACTIONS_SIZE,
                });
            }
        }

        Ok(total)
    }

    fn validate_body_root(&self) -> Result<(), Error>
    where
        Tx: Hashable<Hash = TxHash>,
    {
        if self.header.body_root() != &body_root(&self.uncle_headers, &self.transactions) {
            return Err(Error::BodyRootMismatch);
        }

        Ok(())
    }

    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub const fn uncle_headers(&self) -> &UncleHeaders {
        &self.uncle_headers
    }

    pub fn transactions_iter(&self) -> impl ExactSizeIterator<Item = &Tx> + '_ {
        self.transactions.as_slice().iter()
    }

    #[must_use]
    pub const fn transactions(&self) -> &BlockTransactions<Tx> {
        &self.transactions
    }

    #[must_use]
    pub fn into_transactions(self) -> Vec<Tx> {
        self.transactions.into_inner()
    }

    #[must_use]
    pub fn to_proposal(self) -> Proposal
    where
        Tx: Hashable<Hash = TxHash>,
    {
        Proposal {
            header: self.header,
            uncle_headers: self.uncle_headers,
            references: References::from_block_transactions(&self.transactions),
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
pub fn verify_header_alone(header: &Header) -> Result<(), HeaderError> {
    if header.slot() == Slot::genesis() {
        return Err(HeaderError::GenesisSlot);
    }
    Ok(())
}

/// The commitment to a block body: its uncle headers and its txs
#[must_use]
pub fn body_root<Tx: Hashable<Hash = TxHash>>(
    uncle_headers: &UncleHeaders,
    transactions: &[Tx],
) -> ContentId {
    let mut h = Hasher::new();
    h.update(b"BODY_ROOT_V1");
    h.update(uncle_headers.encode_to_vec());
    h.update(merkle::calculate_transactions_root(transactions));
    ContentId::from(<[u8; 32]>::from(h.finalize()))
}

#[cfg(test)]
mod tests {
    use std::iter;

    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::ZkPublicKey;

    use super::*;
    use crate::mantle::{traits::hashable, transactions::Ops};

    /// The most uncles the tests' blocks may reference.
    pub const TEST_MAX_UNCLES: u8 = 4;

    pub fn create_proof() -> Groth16LeaderProof {
        let (_, private_inputs) = crate::proofs::leader_proof::test_utils::note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::ZERO,
        );
        Groth16LeaderProof::prove(private_inputs, ZkPublicKey::zero())
            .expect("Proof generation should succeed")
    }

    fn create_tx(count: usize) -> Vec<Ops> {
        iter::repeat_with(|| Ops::new_unchecked(vec![]))
            .take(count)
            .collect()
    }

    #[derive(Clone, Copy, Debug)]
    struct IndexedTestMantleTx {
        index: u8,
    }

    impl Hashable for IndexedTestMantleTx {
        const HASHER: hashable::Hasher<Self> = |transaction| TxHash::from([transaction.index; 32]);
        type Hash = TxHash;

        fn as_signing(&self) -> Vec<u8> {
            vec![self.index]
        }
    }

    impl StorageSize for IndexedTestMantleTx {
        fn storage_size(&self) -> usize {
            1
        }
    }

    #[test]
    fn test_block_transaction_count_validation() {
        let parent_block = [0u8; 32].into();
        let slot = Slot::from(42u64);
        let proof_of_leadership = create_proof();

        let transactions = BlockTransactions::empty();
        let _valid_block: Block<Ops> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
        )
        .expect("Valid block should be created");

        let transactions = BlockTransactions::try_from(create_tx(MAX_BLOCK_TRANSACTIONS)).unwrap();
        let _valid_block: Block<Ops> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership,
            transactions,
        )
        .expect("Valid block should be created");

        let invalid_transaction_inputs_result =
            BlockTransactions::<Ops>::try_from(create_tx(MAX_BLOCK_TRANSACTIONS + 1));

        assert!(invalid_transaction_inputs_result.is_err());
        let error = invalid_transaction_inputs_result.unwrap_err();

        match error {
            BoundedError::TooManyItems { count, max } => {
                assert_eq!(count, MAX_BLOCK_TRANSACTIONS + 1);
                assert_eq!(max, MAX_BLOCK_TRANSACTIONS);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn proposal_references_preserve_transaction_hash_prefixes_and_order() {
        let parent_block = [0u8; 32].into();
        let transactions = BlockTransactions::<IndexedTestMantleTx>::try_from(vec![
            IndexedTestMantleTx { index: 1 },
            IndexedTestMantleTx { index: 2 },
            IndexedTestMantleTx { index: 3 },
        ])
        .unwrap();
        let expected_prefixes: Vec<_> = transactions
            .iter()
            .map(|transaction| IndexedTestMantleTx::hash(transaction).prefix())
            .collect();

        let proposal = Block::create(
            parent_block,
            Slot::from(42u64),
            UncleHeaders::empty(),
            create_proof(),
            transactions,
        )
        .unwrap()
        .to_proposal();

        assert_eq!(
            proposal.mempool_transactions(),
            expected_prefixes.as_slice()
        );
    }

    #[test]
    fn proposal_accepts_maximum_transaction_references() {
        let parent_block = [0u8; 32].into();
        let block = Block::create(
            parent_block,
            Slot::from(42u64),
            UncleHeaders::empty(),
            create_proof(),
            BlockTransactions::<Ops>::try_from(create_tx(MAX_BLOCK_TRANSACTIONS)).unwrap(),
        )
        .unwrap();

        let proposal = block.to_proposal();

        assert_eq!(
            proposal.mempool_transactions().len(),
            MAX_BLOCK_TRANSACTIONS
        );
    }

    #[test]
    fn proposal_deserialization_rejects_excess_transaction_references() {
        #[derive(Serialize)]
        struct LegacyReferences {
            mempool_transactions: Vec<TxHashPrefix>,
        }

        #[derive(Serialize)]
        struct LegacyProposal {
            header: Header,
            uncle_headers: UncleHeaders,
            references: LegacyReferences,
        }

        let proposal = Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            UncleHeaders::empty(),
            create_proof(),
            BlockTransactions::<Ops>::empty(),
        )
        .unwrap()
        .to_proposal();
        let legacy = LegacyProposal {
            header: proposal.header.clone(),
            uncle_headers: proposal.uncle_headers,
            references: LegacyReferences {
                mempool_transactions: vec![TxHashPrefix::default(); MAX_BLOCK_TRANSACTIONS + 1],
            },
        };
        let bytes = bincode::serialize(&legacy).unwrap();

        let error = <Proposal as lb_binary_codec::bincode::DeserializeOp>::from_bytes(&bytes)
            .expect_err("proposal with too many transaction references must be rejected");

        assert!(
            error.to_string().contains("exceeds maximum"),
            "unexpected deserialization error: {error}"
        );
    }

    #[derive(Clone, Copy, Debug)]
    struct TestMantleTx<const SIZE: usize>;

    impl<const SIZE: usize> Hashable for TestMantleTx<SIZE> {
        //noinspection RsTypeCheck: The type is correct, but the linter is confused by
        // the closure.
        const HASHER: hashable::Hasher<Self> = |_tx| TxHash::from([0u8; 32]);
        type Hash = TxHash;

        fn as_signing(&self) -> Vec<u8> {
            vec![0u8]
        }
    }

    impl<const SIZE: usize> StorageSize for TestMantleTx<SIZE> {
        fn storage_size(&self) -> usize {
            SIZE
        }
    }

    #[test]
    fn test_block_transaction_size_validation() {
        let parent_block = [0u8; 32].into();
        let slot = Slot::from(42u64);
        let proof_of_leadership = create_proof();

        let transactions = BlockTransactions::empty();
        let _valid_block: Block<Ops> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
        )
        .expect("Valid block should be created");

        let transactions: BlockTransactions<TestMantleTx<MAX_BLOCK_TRANSACTIONS_SIZE>> =
            BlockTransactions::try_from(vec![TestMantleTx::<MAX_BLOCK_TRANSACTIONS_SIZE>]).unwrap();
        let _valid_block = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
        )
        .expect("Valid block should be created");

        let oversized: BlockTransactions<TestMantleTx<{ MAX_BLOCK_TRANSACTIONS_SIZE + 1 }>> =
            BlockTransactions::try_from(vec![TestMantleTx::<{ MAX_BLOCK_TRANSACTIONS_SIZE + 1 }>])
                .unwrap();
        let invalid_transaction_inputs_result = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership,
            oversized,
        );

        assert!(invalid_transaction_inputs_result.is_err());
        let error = invalid_transaction_inputs_result.unwrap_err();

        match error {
            Error::ContentTooBig { size, max } => {
                assert_eq!(size, MAX_BLOCK_TRANSACTIONS_SIZE + 1);
                assert_eq!(max, MAX_BLOCK_TRANSACTIONS_SIZE);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn global_block_limits_are_reflective_in_block_transaction_bounds() {
        assert_eq!(BlockTransactions::<Ops>::MIN, 0);
        assert_eq!(BlockTransactions::<Ops>::MAX, MAX_BLOCK_TRANSACTIONS);
    }

    #[test]
    fn test_create_rejects_genesis_slot() {
        let parent_block = [0u8; 32].into();
        let proof = create_proof();

        // Build a syntactically valid non-genesis block first.
        let txs = BlockTransactions::<Ops>::empty();
        let block_result = Block::create(
            parent_block,
            Slot::from(0u64),
            UncleHeaders::empty(),
            proof,
            txs,
        )
        .unwrap_err();

        assert!(matches!(
            block_result,
            Error::Header(HeaderError::GenesisSlot)
        ));
    }

    #[test]
    fn test_reconstruct_rejects_genesis_slot() {
        let parent_block = [0u8; 32].into();
        let proof = create_proof();

        // Create a valid NON-genesis block first so we can reuse a valid signature
        // shape.
        let valid = Block::create(
            parent_block,
            Slot::from(1u64),
            UncleHeaders::empty(),
            proof.clone(),
            BlockTransactions::<Ops>::empty(),
        )
        .expect("valid non-genesis block");

        // Rebuild header at genesis slot and sign it so signature itself is still
        // consistent.
        let genesis_header = Header::new(
            parent_block,
            *valid.header().body_root(),
            Slot::genesis(),
            proof,
        );

        let err = Block::reconstruct(
            genesis_header,
            UncleHeaders::empty(),
            BlockTransactions::<Ops>::empty(),
        )
        .expect_err("genesis slot must be rejected by reconstruct path");

        assert!(matches!(err, Error::Header(HeaderError::GenesisSlot)));
    }

    /// The maximum-size proposal of an era matches the era's size bound.
    #[test]
    fn maximum_proposal_matches_the_specified_size() {
        use lb_binary_codec::canonical::BinaryEncode as _;

        let proof = create_proof();
        let uncles = (0..TEST_MAX_UNCLES)
            .map(|slot| uncle_header(u64::from(slot), &proof))
            .collect();
        let proposal = Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            UncleHeaders::try_new(uncles, TEST_MAX_UNCLES).unwrap(),
            proof,
            BlockTransactions::<Ops>::try_from(create_tx(MAX_BLOCK_TRANSACTIONS)).unwrap(),
        )
        .expect("valid block")
        .to_proposal();

        let max_size = Proposal::max_encoded_size(TEST_MAX_UNCLES);
        assert_eq!(proposal.encoded_length(), max_size);
        assert_eq!(proposal.encode().len(), max_size);
        // Under 4 uncles, the size Blend pads every payload to.
        assert_eq!(max_size, 17_712);
    }

    #[test]
    fn maximum_proposal_size_follows_the_header() {
        assert_eq!(
            UncleHeaders::max_encoded_size(TEST_MAX_UNCLES),
            1 + usize::from(TEST_MAX_UNCLES) * Header::CANONICAL_ENCODED_SIZE
        );
        // 265 byte header, 4 uncle headers, 1024 references.
        assert_eq!(
            Proposal::max_encoded_size(TEST_MAX_UNCLES),
            265 + 1 + 4 * 265 + 16_386
        );
        assert_eq!(Groth16LeaderProof::CANONICAL_ENCODED_SIZE, 192);
        assert_eq!(Header::CANONICAL_ENCODED_SIZE, 265);
        assert_eq!(Proposal::max_encoded_size(TEST_MAX_UNCLES), 17_712);
    }

    #[test]
    fn body_root_accepts_carried_uncle_headers() {
        let proof = create_proof();
        let uncles = UncleHeaders::try_new(
            vec![uncle_header(1, &proof), uncle_header(2, &proof)],
            TEST_MAX_UNCLES,
        )
        .unwrap();

        block_with_uncles(uncles, proof)
            .into_verified()
            .expect("the carried headers are the ones the body root commits to");
    }

    #[test]
    fn body_root_rejects_dropped_uncle_header() {
        let proof = create_proof();
        let uncles = UncleHeaders::try_new(vec![uncle_header(1, &proof)], TEST_MAX_UNCLES).unwrap();
        let mut block = block_with_uncles(uncles, proof);

        block.uncle_headers = UncleHeaders::empty();

        assert!(matches!(
            block.into_verified(),
            Err(Error::BodyRootMismatch)
        ));
    }

    #[test]
    fn body_root_rejects_substituted_uncle_header() {
        let proof = create_proof();
        let uncles = UncleHeaders::try_new(vec![uncle_header(1, &proof)], TEST_MAX_UNCLES).unwrap();
        let mut block = block_with_uncles(uncles, proof.clone());

        // Same count, but a different header than the one committed to.
        block.uncle_headers =
            UncleHeaders::try_new(vec![uncle_header(2, &proof)], TEST_MAX_UNCLES).unwrap();

        assert!(matches!(
            block.into_verified(),
            Err(Error::BodyRootMismatch)
        ));
    }

    #[test]
    fn body_root_rejects_reordered_uncle_headers() {
        let proof = create_proof();
        let (first, second) = (uncle_header(1, &proof), uncle_header(2, &proof));
        let mut block = block_with_uncles(
            UncleHeaders::try_new(vec![first.clone(), second.clone()], TEST_MAX_UNCLES).unwrap(),
            proof,
        );

        block.uncle_headers = UncleHeaders::try_new(vec![second, first], TEST_MAX_UNCLES).unwrap();

        assert!(matches!(
            block.into_verified(),
            Err(Error::BodyRootMismatch)
        ));
    }

    fn uncle_header(slot: u64, proof: &Groth16LeaderProof) -> Header {
        Header::new(
            HeaderId::from([9u8; 32]),
            ContentId::from([9u8; 32]),
            Slot::from(slot),
            proof.clone(),
        )
    }

    fn block_with_uncles(uncle_headers: UncleHeaders, proof: Groth16LeaderProof) -> Block<Ops> {
        Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            uncle_headers,
            proof,
            BlockTransactions::empty(),
        )
        .expect("block creation should succeed")
    }
}
