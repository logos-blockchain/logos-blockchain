mod deser;
mod fixtures;
pub mod genesis;
mod uncle;

use core::fmt::Debug;

use lb_binary_codec::{
    bincode::SerializeOp as _,
    canonical::{BinaryDecode, BinaryEncode, DecodeError},
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};
use lb_utils::bounded::{BoundedError, UpperRuntimeBoundedVec};
use serde::{Deserialize, Serialize};
pub use uncle::{SignedHeader, UncleHeaders};

use crate::{
    crypto::{Digest as _, Hasher},
    header::{ContentId, Header, HeaderId},
    mantle::{
        traits::{Hashable, StorageSize},
        transactions::hash::{TxHash, TxHashPrefix},
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof as _},
    utils::merkle,
};

pub type BlockNumber = u64;

/// The most uncles, transactions and transaction bytes the era of a block
/// allows: what the block, or a proposal of it, is built and decoded under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockLimits {
    /// The most uncles a block may reference.
    pub max_uncles: u8,
    /// The most transactions a block may hold.
    pub max_transactions: u16,
    /// The most bytes a block's transactions may take in their canonical
    /// encoding, their count aside. Not the size of the whole block.
    pub max_transactions_size: u32,
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, BinaryEncode)]
pub struct Proposal {
    pub header: Header,
    pub uncle_headers: UncleHeaders,
    pub references: References,
    pub signature: Ed25519Signature,
}

impl BinaryDecode for Proposal {
    /// The limits of the era of the proposal's slot.
    type Context = BlockLimits;

    fn decode<'input>(
        input: &'input [u8],
        limits: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, header) = Header::decode(input, &())?;
        let (input, uncle_headers) = UncleHeaders::decode(input, &limits.max_uncles)?;
        let (input, references) = References::decode(input, &limits.max_transactions)?;
        let (input, signature) = Ed25519Signature::decode(input, &())?;
        Ok((
            input,
            Self {
                header,
                uncle_headers,
                references,
                signature,
            },
        ))
    }
}

/// Transaction-hash prefixes referenced by a block proposal, at most as many as
/// the era of the proposal allows transactions.
pub type BlockTransactionReferences = UpperRuntimeBoundedVec<TxHashPrefix, u16>;

/// References to transactions that are included in a block proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, BinaryEncode)]
pub struct References {
    /// Bounded hashes of the transactions that are included in the block
    /// proposal.
    pub mempool_transactions: BlockTransactionReferences,
}

impl BinaryDecode for References {
    /// The most transactions the era of the proposal allows, which decoding
    /// holds the count to.
    type Context = u16;

    fn decode<'input>(
        input: &'input [u8],
        max_transactions: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, mempool_transactions) =
            BlockTransactionReferences::decode(input, &(*max_transactions, ()))?;
        Ok((
            input,
            Self {
                mempool_transactions,
            },
        ))
    }
}

impl References {
    /// The largest canonical encoding of the references of a proposal whose
    /// era allows `max_transactions`.
    #[must_use]
    pub fn max_encoded_size(max_transactions: u16) -> usize {
        size_of::<u16>() + usize::from(max_transactions) * TxHashPrefix::CANONICAL_ENCODED_SIZE
    }

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
    signature: Ed25519Signature,
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
            signature: Ed25519Signature,
            uncle_headers: UncleHeaders,
            transactions: BlockTransactions<Tx>,
        }

        let raw = RawBlock::<Tx>::deserialize(deserializer)?;

        // Serde reads blocks a node serves through its API, already checked
        // against their era by that node, so it holds them to no era: their
        // transactions to the most bytes an era's limit can be.
        Self::reconstruct(
            raw.header,
            raw.uncle_headers,
            raw.transactions,
            raw.signature,
            u32::MAX,
        )
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
            signature,
            uncle_headers,
            transactions,
        } = self;

        header.encoded_length()
            + uncle_headers.encoded_length()
            + transactions.encoded_length()
            + signature.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            header,
            signature,
            uncle_headers,
            transactions,
        } = self;

        header.encode_into(out);
        uncle_headers.encode_into(out);
        transactions.encode_into(out);
        signature.encode_into(out);
    }
}

// Decoding checks the block as `reconstruct` does.
impl<Tx> BinaryDecode for Block<Tx>
where
    Tx: BinaryDecode + Hashable<Hash = TxHash> + StorageSize,
{
    /// The most uncles the era of the block's slot allows, the most bytes that
    /// era allows the block's transactions, then the context of its
    /// transactions: the most transactions that era allows, with the
    /// transactions' own context.
    type Context = (u8, u32, (u16, Tx::Context));

    fn decode<'input>(
        input: &'input [u8],
        (max_uncles, max_transactions_size, transactions_context): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, header) = Header::decode(input, &())?;
        let (input, uncle_headers) = UncleHeaders::decode(input, max_uncles)?;
        let (input, transactions) = BlockTransactions::<Tx>::decode(input, transactions_context)?;
        let (input, signature) = Ed25519Signature::decode(input, &())?;
        let block = Self::reconstruct(
            header,
            uncle_headers,
            transactions,
            signature,
            *max_transactions_size,
        )
        .map_err(|error| DecodeError::invalid_value::<Self>(error.to_string()))?;
        Ok((input, block))
    }
}

impl Proposal {
    /// Maximum canonical encoded size of a block proposal whose era has
    /// `limits`.
    ///
    /// The bound is composed from the maxima owned by each component rather
    /// than being an unexplained total. It describes the canonical encoding,
    /// not the configured-bincode representation.
    #[must_use]
    pub fn max_encoded_size(limits: BlockLimits) -> usize {
        Header::CANONICAL_ENCODED_SIZE
            + UncleHeaders::max_encoded_size(limits.max_uncles)
            + References::max_encoded_size(limits.max_transactions)
            + Ed25519Signature::CANONICAL_ENCODED_SIZE
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

    #[must_use]
    pub const fn signature(&self) -> &Ed25519Signature {
        &self.signature
    }
}

/// Validated transaction payload for blocks: at most as many transactions as
/// the era of the block allows.
///
/// The count is checked against the era's maximum, a `u16`, when the
/// transactions are built or decoded. Serde reads them without an era, under
/// the largest maximum of the type.
pub type BlockTransactions<Tx> = UpperRuntimeBoundedVec<Tx, u16>;

impl<Tx> Block<Tx> {
    /// A block of `slot`, whose era allows its transactions
    /// `max_transactions_size` bytes.
    pub fn create(
        parent_block: HeaderId,
        slot: Slot,
        uncle_headers: UncleHeaders,
        proof_of_leadership: Groth16LeaderProof,
        transactions: BlockTransactions<Tx>,
        max_transactions_size: u32,
        signing_key: &Ed25519Key,
    ) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        // 1. Non-genesis blocks only
        if slot == Slot::genesis() {
            return Err(HeaderError::GenesisSlot.into());
        }

        // 2. Expected leader public key
        let expected_leader_public_key = proof_of_leadership.leader_key();
        if expected_leader_public_key != signing_key.public_key().as_unverified() {
            return Err(Error::KeyMismatch);
        }

        // 3. Body root & header
        let header = Header::new(
            parent_block,
            body_root(&uncle_headers, transactions.as_slice()),
            slot,
            proof_of_leadership,
        );

        // 4. Signature over the header
        let signature = header.sign(signing_key)?;

        // 5. New block
        let block = Self {
            header,
            signature,
            uncle_headers,
            transactions,
        };

        // 6. Size is ok
        block.validate_total_transactions_size(max_transactions_size)?;

        Ok(block)
    }

    /// A block rebuilt from its parts, checked as an era that allows its
    /// transactions `max_transactions_size` bytes does.
    pub fn reconstruct(
        header: Header,
        uncle_headers: UncleHeaders,
        transactions: BlockTransactions<Tx>,
        signature: Ed25519Signature,
        max_transactions_size: u32,
    ) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        let block = Self {
            header,
            signature,
            uncle_headers,
            transactions,
        };
        let block = block.into_verified(max_transactions_size)?;

        Ok(block)
    }

    fn into_verified(self, max_transactions_size: u32) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        // 1. Checks that need the header alone
        verify_header_alone(&self.header)?;

        // 2. Size is ok
        self.validate_total_transactions_size(max_transactions_size)?;

        // 3. Body root matches the carried uncle headers and transactions
        self.validate_body_root()?;

        // 4. Signature is valid over the header bytes
        verify_header_signature(&self.header, &self.signature)?;

        Ok(self)
    }

    /// Maximum canonical encoded size of a block whose era has `limits`: its
    /// transactions take at most `limits.max_transactions_size` bytes besides
    /// their count.
    #[must_use]
    pub fn max_encoded_size(limits: BlockLimits) -> usize {
        Header::CANONICAL_ENCODED_SIZE
            + UncleHeaders::max_encoded_size(limits.max_uncles)
            + size_of::<u16>()
            + limits.max_transactions_size as usize
            + Ed25519Signature::CANONICAL_ENCODED_SIZE
    }

    fn validate_total_transactions_size(&self, max_transactions_size: u32) -> Result<usize, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        let max = max_transactions_size as usize;
        let mut total = 0usize;

        for item in &self.transactions {
            total = total
                .checked_add(item.storage_size())
                .ok_or(Error::ContentTooBig {
                    size: usize::MAX,
                    max,
                })?;

            if total > max {
                return Err(Error::ContentTooBig { size: total, max });
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
    pub const fn signature(&self) -> &Ed25519Signature {
        &self.signature
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
            signature: self.signature,
        }
    }
}

/// A transaction carried by a block, with the slot of that block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxInBlock<Tx> {
    pub tx: Tx,
    pub slot: Slot,
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

/// Verifies the signature of block header.
pub fn verify_header_signature(header: &Header, signature: &Ed25519Signature) -> Result<(), Error> {
    let header_bytes = header.to_bytes()?;
    header
        .leader_proof()
        .leader_key()
        .verify(&header_bytes, signature)
        .map_err(|_| Error::Signature)
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

    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::UnsecuredZkKey;
    use lb_pol::LotteryConstants;
    use lb_utils::math::ProperFraction;
    use lb_utxotree::UtxoTree;

    use super::*;
    use crate::{
        crypto::ZkHasher,
        mantle::{
            fixtures::ops::op_values::FORK_DIGEST,
            ledger::{Note, Utxo},
            ops::leader_claim::VoucherCm,
            traits::hashable,
            transactions::{Ops, TxList},
        },
        proofs::leader_proof::{LeaderPrivate, LeaderPublic},
    };

    /// The most uncles the tests' blocks may reference.
    pub const TEST_MAX_UNCLES: u8 = 4;
    /// The most transactions the tests' blocks may hold.
    pub const TEST_MAX_TRANSACTIONS: u16 = 1024;
    /// The most bytes the tests' blocks' transactions may take.
    pub const TEST_MAX_TRANSACTIONS_SIZE: u32 = 2 * 1024 * 1024;

    pub fn create_proof() -> Groth16LeaderProof {
        let leader_sk = UnsecuredZkKey::zero();
        let utxo = Utxo {
            op_id: [0u8; 32],
            output_index: 0,
            note: Note::new(1000, leader_sk.to_public_key()),
        };
        let utxo_tree = UtxoTree::<_, _, ZkHasher>::new().insert(utxo.id(), utxo).0;
        let utxo_tree_root = utxo_tree.root();
        let utxo_merkle_path = utxo_tree.path(&utxo.id()).expect("note must exist in tree");

        let (lottery_0, lottery_1) =
            LotteryConstants::new(ProperFraction::new(1, 10.try_into().unwrap()).unwrap())
                .compute_lottery_values(1000);

        // We grind the nonce here to find a winning PoL
        let public_inputs = {
            let mut nonce = 0;
            while nonce < 1000 {
                let inputs = LeaderPublic::new(
                    utxo_tree_root,
                    utxo_tree_root,
                    Fr::from(nonce),
                    0,
                    lottery_0,
                    lottery_1,
                );

                if inputs.check_winning(utxo.note.value, *utxo.id().as_fr(), *leader_sk.as_fr()) {
                    break;
                }

                nonce += 1;
            }
            LeaderPublic::new(
                utxo_tree_root,
                utxo_tree_root,
                Fr::from(nonce),
                0,
                lottery_0,
                lottery_1,
            )
        };

        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let verifying_key = signing_key.public_key();

        let private_inputs = LeaderPrivate::new(
            public_inputs,
            utxo,
            &utxo_merkle_path, // aged path
            &utxo_merkle_path, // latest path
            *leader_sk.as_fr(),
            &verifying_key,
        );
        Groth16LeaderProof::prove(private_inputs, VoucherCm::default())
            .expect("Proof generation should succeed")
    }

    fn create_tx(count: usize) -> Vec<Ops> {
        iter::repeat_with(|| Ops::new(FORK_DIGEST, TxList::new_unchecked(vec![])))
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
    fn test_block_signature_validation() {
        let parent_block = [0u8; 32].into();
        let slot = Slot::from(42u64);
        let proof_of_leadership = create_proof();
        let transactions = BlockTransactions::<Ops>::empty();

        let valid_signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let valid_block = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership,
            transactions.clone(),
            TEST_MAX_TRANSACTIONS_SIZE,
            &valid_signing_key,
        )
        .expect("Valid block should be created");

        let header = valid_block.header().clone();
        let valid_signature = *valid_block.signature();

        let _reconstructed_block = Block::reconstruct(
            header.clone(),
            UncleHeaders::empty(),
            transactions.clone(),
            valid_signature,
            TEST_MAX_TRANSACTIONS_SIZE,
        )
        .expect("Should reconstruct block with valid signature");

        let wrong_signing_key = Ed25519Key::from_bytes(&[1u8; 32]);
        let invalid_signature = header
            .sign(&wrong_signing_key)
            .expect("Signing should work");

        let invalid_block_result = Block::reconstruct(
            header,
            UncleHeaders::empty(),
            transactions,
            invalid_signature,
            TEST_MAX_TRANSACTIONS_SIZE,
        );

        assert!(
            invalid_block_result.is_err(),
            "Should not reconstruct block with invalid signature"
        );
    }

    #[test]
    fn test_block_transaction_count_validation() {
        let parent_block = [0u8; 32].into();
        let slot = Slot::from(42u64);
        let proof_of_leadership = create_proof();
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);

        let transactions = BlockTransactions::empty();
        let _valid_block: Block<Ops> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
            TEST_MAX_TRANSACTIONS_SIZE,
            &signing_key,
        )
        .expect("Valid block should be created");

        let transactions = BlockTransactions::try_new(
            create_tx(usize::from(TEST_MAX_TRANSACTIONS)),
            TEST_MAX_TRANSACTIONS,
        )
        .unwrap();
        let _valid_block: Block<Ops> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership,
            transactions,
            TEST_MAX_TRANSACTIONS_SIZE,
            &signing_key,
        )
        .expect("Valid block should be created");

        let invalid_transaction_inputs_result = BlockTransactions::<Ops>::try_new(
            create_tx(usize::from(TEST_MAX_TRANSACTIONS) + 1),
            TEST_MAX_TRANSACTIONS,
        );

        assert!(invalid_transaction_inputs_result.is_err());
        let error = invalid_transaction_inputs_result.unwrap_err();

        match error {
            BoundedError::TooManyItems { count, max } => {
                assert_eq!(count, usize::from(TEST_MAX_TRANSACTIONS) + 1);
                assert_eq!(max, usize::from(TEST_MAX_TRANSACTIONS));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn proposal_references_preserve_transaction_hash_prefixes_and_order() {
        let parent_block = [0u8; 32].into();
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let transactions = BlockTransactions::<IndexedTestMantleTx>::try_new(
            vec![
                IndexedTestMantleTx { index: 1 },
                IndexedTestMantleTx { index: 2 },
                IndexedTestMantleTx { index: 3 },
            ],
            TEST_MAX_TRANSACTIONS,
        )
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
            TEST_MAX_TRANSACTIONS_SIZE,
            &signing_key,
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
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let block = Block::create(
            parent_block,
            Slot::from(42u64),
            UncleHeaders::empty(),
            create_proof(),
            BlockTransactions::<Ops>::try_new(
                create_tx(usize::from(TEST_MAX_TRANSACTIONS)),
                TEST_MAX_TRANSACTIONS,
            )
            .unwrap(),
            TEST_MAX_TRANSACTIONS_SIZE,
            &signing_key,
        )
        .unwrap();

        let proposal = block.to_proposal();

        assert_eq!(
            proposal.mempool_transactions().len(),
            usize::from(TEST_MAX_TRANSACTIONS)
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
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);

        let transactions = BlockTransactions::empty();
        let _valid_block: Block<Ops> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
            TEST_MAX_TRANSACTIONS_SIZE,
            &signing_key,
        )
        .expect("Valid block should be created");

        let transactions: BlockTransactions<TestMantleTx<{ TEST_MAX_TRANSACTIONS_SIZE as usize }>> =
            BlockTransactions::try_new(
                vec![TestMantleTx::<{ TEST_MAX_TRANSACTIONS_SIZE as usize }>],
                TEST_MAX_TRANSACTIONS,
            )
            .unwrap();
        let _valid_block = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
            TEST_MAX_TRANSACTIONS_SIZE,
            &signing_key,
        )
        .expect("Valid block should be created");

        let oversized: BlockTransactions<
            TestMantleTx<{ TEST_MAX_TRANSACTIONS_SIZE as usize + 1 }>,
        > = BlockTransactions::try_new(
            vec![TestMantleTx::<{ TEST_MAX_TRANSACTIONS_SIZE as usize + 1 }>],
            TEST_MAX_TRANSACTIONS,
        )
        .unwrap();
        let invalid_transaction_inputs_result = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership,
            oversized,
            TEST_MAX_TRANSACTIONS_SIZE,
            &signing_key,
        );

        assert!(invalid_transaction_inputs_result.is_err());
        let error = invalid_transaction_inputs_result.unwrap_err();

        match error {
            Error::ContentTooBig { size, max } => {
                assert_eq!(size, TEST_MAX_TRANSACTIONS_SIZE as usize + 1);
                assert_eq!(max, TEST_MAX_TRANSACTIONS_SIZE as usize);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_create_rejects_genesis_slot() {
        let parent_block = [0u8; 32].into();
        let proof = create_proof();

        // Build a syntactically valid non-genesis block first.
        let txs = BlockTransactions::<Ops>::empty();
        let key = Ed25519Key::from_bytes(&[0; 32]);
        let block_result = Block::create(
            parent_block,
            Slot::from(0u64),
            UncleHeaders::empty(),
            proof,
            txs,
            TEST_MAX_TRANSACTIONS_SIZE,
            &key,
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
        let key = Ed25519Key::from_bytes(&[0; 32]);

        // Create a valid NON-genesis block first so we can reuse a valid signature
        // shape.
        let valid = Block::create(
            parent_block,
            Slot::from(1u64),
            UncleHeaders::empty(),
            proof.clone(),
            BlockTransactions::<Ops>::empty(),
            TEST_MAX_TRANSACTIONS_SIZE,
            &key,
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
        let genesis_signature = genesis_header
            .sign(&key)
            .expect("header signing should succeed");

        let err = Block::reconstruct(
            genesis_header,
            UncleHeaders::empty(),
            BlockTransactions::<Ops>::empty(),
            genesis_signature,
            TEST_MAX_TRANSACTIONS_SIZE,
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
            .map(|slot| signed_uncle(u64::from(slot), &proof))
            .collect();
        let proposal = Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            UncleHeaders::try_new(uncles, TEST_MAX_UNCLES).unwrap(),
            proof,
            BlockTransactions::<Ops>::try_new(
                create_tx(usize::from(TEST_MAX_TRANSACTIONS)),
                TEST_MAX_TRANSACTIONS,
            )
            .unwrap(),
            TEST_MAX_TRANSACTIONS_SIZE,
            &Ed25519Key::from_bytes(&[0; 32]),
        )
        .expect("valid block")
        .to_proposal();

        let max_size = Proposal::max_encoded_size(BlockLimits {
            max_uncles: TEST_MAX_UNCLES,
            max_transactions: TEST_MAX_TRANSACTIONS,
            max_transactions_size: TEST_MAX_TRANSACTIONS_SIZE,
        });
        assert_eq!(proposal.encoded_length(), max_size);
        assert_eq!(proposal.encode().len(), max_size);
        // Under 4 uncles, the size Blend pads every payload to.
        assert_eq!(max_size, 18_187);
    }

    #[test]
    fn body_root_accepts_carried_uncle_headers() {
        let proof = create_proof();
        let uncles = UncleHeaders::try_new(
            vec![signed_uncle(1, &proof), signed_uncle(2, &proof)],
            TEST_MAX_UNCLES,
        )
        .unwrap();

        block_with_uncles(uncles, proof)
            .into_verified(TEST_MAX_TRANSACTIONS_SIZE)
            .expect("the carried headers are the ones the body root commits to");
    }

    #[test]
    fn body_root_rejects_dropped_uncle_header() {
        let proof = create_proof();
        let uncles = UncleHeaders::try_new(vec![signed_uncle(1, &proof)], TEST_MAX_UNCLES).unwrap();
        let mut block = block_with_uncles(uncles, proof);

        block.uncle_headers = UncleHeaders::empty();

        assert!(matches!(
            block.into_verified(TEST_MAX_TRANSACTIONS_SIZE),
            Err(Error::BodyRootMismatch)
        ));
    }

    #[test]
    fn body_root_rejects_substituted_uncle_header() {
        let proof = create_proof();
        let uncles = UncleHeaders::try_new(vec![signed_uncle(1, &proof)], TEST_MAX_UNCLES).unwrap();
        let mut block = block_with_uncles(uncles, proof.clone());

        // Same count, but a different header than the one committed to.
        block.uncle_headers =
            UncleHeaders::try_new(vec![signed_uncle(2, &proof)], TEST_MAX_UNCLES).unwrap();

        assert!(matches!(
            block.into_verified(TEST_MAX_TRANSACTIONS_SIZE),
            Err(Error::BodyRootMismatch)
        ));
    }

    #[test]
    fn body_root_rejects_reordered_uncle_headers() {
        let proof = create_proof();
        let (first, second) = (signed_uncle(1, &proof), signed_uncle(2, &proof));
        let mut block = block_with_uncles(
            UncleHeaders::try_new(vec![first.clone(), second.clone()], TEST_MAX_UNCLES).unwrap(),
            proof,
        );

        block.uncle_headers = UncleHeaders::try_new(vec![second, first], TEST_MAX_UNCLES).unwrap();

        assert!(matches!(
            block.into_verified(TEST_MAX_TRANSACTIONS_SIZE),
            Err(Error::BodyRootMismatch)
        ));
    }

    #[test]
    fn body_root_rejects_tampered_uncle_signature() {
        let proof = create_proof();
        let uncle = signed_uncle(1, &proof);
        let mut block = block_with_uncles(
            UncleHeaders::try_new(vec![uncle.clone()], TEST_MAX_UNCLES).unwrap(),
            proof,
        );

        // Replace only the signature, leaving the header it signs untouched.
        let other_signature = uncle
            .header()
            .sign(&Ed25519Key::from_bytes(&[1; 32]))
            .expect("header signing should succeed");
        block.uncle_headers = UncleHeaders::try_new(
            vec![SignedHeader::new(uncle.header().clone(), other_signature)],
            TEST_MAX_UNCLES,
        )
        .unwrap();

        assert!(matches!(
            block.into_verified(TEST_MAX_TRANSACTIONS_SIZE),
            Err(Error::BodyRootMismatch)
        ));
    }

    fn signed_uncle(slot: u64, proof: &Groth16LeaderProof) -> SignedHeader {
        let header = Header::new(
            HeaderId::from([9u8; 32]),
            ContentId::from([9u8; 32]),
            Slot::from(slot),
            proof.clone(),
        );
        let signature = header
            .sign(&Ed25519Key::from_bytes(&[0; 32]))
            .expect("header signing should succeed");
        SignedHeader::new(header, signature)
    }

    fn block_with_uncles(uncle_headers: UncleHeaders, proof: Groth16LeaderProof) -> Block<Ops> {
        Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            uncle_headers,
            proof,
            BlockTransactions::empty(),
            TEST_MAX_TRANSACTIONS_SIZE,
            &Ed25519Key::from_bytes(&[0; 32]),
        )
        .expect("block creation should succeed")
    }
}
