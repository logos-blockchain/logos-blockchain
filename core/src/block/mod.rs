mod deser;
mod fixtures;
pub mod genesis;
mod uncle;

use core::fmt::Debug;

use bytes::Bytes;
use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};
use lb_utils::bounded::{BoundedError, BoundedVec, UpperBoundedVec};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
pub use uncle::{SignedHeader, UncleHeaders};

use crate::{
    codec::{DeserializeOp as _, SerializeOp as _},
    crypto::{Digest as _, Hasher},
    header::{ContentId, Header, HeaderId},
    mantle::{
        traits::{Hashable, StorageSize},
        transactions::hash::{TxHash, TxHashPrefix},
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof as _},
    utils::merkle,
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
    Serialisation(#[from] crate::codec::Error),
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
    #[error("Signature error.")]
    Signature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BinaryCodec)]
pub struct Proposal {
    pub header: Header,
    pub uncle_headers: UncleHeaders,
    pub references: References,
    pub signature: Ed25519Signature,
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

#[derive(Clone, Debug, Serialize)]
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

        Self::reconstruct(
            raw.header,
            raw.uncle_headers,
            raw.transactions,
            raw.signature,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl Proposal {
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
        if expected_leader_public_key != &signing_key.public_key() {
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
        block.validate_total_transactions_size()?;

        Ok(block)
    }

    pub fn reconstruct(
        header: Header,
        uncle_headers: UncleHeaders,
        transactions: BlockTransactions<Tx>,
        signature: Ed25519Signature,
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
        let block = block.into_verified()?;

        Ok(block)
    }

    fn into_verified(self) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        // 1. Checks that need the header alone
        verify_header_alone(&self.header, &self.signature)?;

        // 2. Size is ok
        self.validate_total_transactions_size()?;

        // 3. Body root matches the carried uncle headers and transactions
        self.validate_body_root()?;

        Ok(self)
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

    #[must_use]
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

/// The checks that the header and the signature over it settle on their own:
/// the header's slot must not be the genesis one, and the signature must verify
/// by the leader key.
///
/// This does not check `body_root` because it commits to a body this function
/// does not have. It should be checked separately by the caller.
pub fn verify_header_alone(
    header: &Header,
    signature: &Ed25519Signature,
) -> Result<(), HeaderError> {
    if header.slot() == Slot::genesis() {
        return Err(HeaderError::GenesisSlot);
    }

    let header_bytes = header.to_bytes().map_err(|_| HeaderError::Signature)?;
    header
        .leader_proof()
        .leader_key()
        .verify(&header_bytes, signature)
        .map_err(|_| HeaderError::Signature)
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

impl<Tx: Clone + Eq + Serialize + DeserializeOwned + Hashable<Hash = TxHash> + StorageSize>
    TryFrom<Bytes> for Block<Tx>
{
    type Error = crate::codec::Error;

    fn try_from(bytes: Bytes) -> Result<Self, Self::Error> {
        let block = Self::from_bytes(&bytes)?;

        let block = block
            .into_verified()
            .map_err(|e| crate::codec::Error::Deserialize(Box::new(e)))?;
        Ok(block)
    }
}

impl<Tx: Clone + Eq + Serialize + DeserializeOwned + Hashable<Hash = TxHash>> TryFrom<Block<Tx>>
    for Bytes
{
    type Error = crate::codec::Error;

    fn try_from(block: Block<Tx>) -> Result<Self, Self::Error> {
        block.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use std::iter;

    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::UnsecuredZkKey;
    use lb_pol::LotteryConstants;
    use lb_utils::math::NonNegativeRatio;
    use lb_utxotree::UtxoTree;

    use super::*;
    use crate::{
        crypto::ZkHasher,
        mantle::{
            ledger::{Note, Utxo},
            ops::leader_claim::VoucherCm,
            traits::hashable,
            transactions::{Ops, mantle_tx::RawMantleTx},
        },
        proofs::leader_proof::{LeaderPrivate, LeaderPublic},
    };

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
            LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
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

    fn create_tx(count: usize) -> Vec<RawMantleTx> {
        iter::repeat_with(|| RawMantleTx(Ops::new_unchecked(vec![])))
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
        let transactions = BlockTransactions::<RawMantleTx>::empty();

        let valid_signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let valid_block = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership,
            transactions.clone(),
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
        let _valid_block: Block<RawMantleTx> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
            &signing_key,
        )
        .expect("Valid block should be created");

        let transactions = BlockTransactions::try_from(create_tx(MAX_BLOCK_TRANSACTIONS)).unwrap();
        let _valid_block: Block<RawMantleTx> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership,
            transactions,
            &signing_key,
        )
        .expect("Valid block should be created");

        let invalid_transaction_inputs_result =
            BlockTransactions::<RawMantleTx>::try_from(create_tx(MAX_BLOCK_TRANSACTIONS + 1));

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
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
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
            BlockTransactions::<RawMantleTx>::try_from(create_tx(MAX_BLOCK_TRANSACTIONS)).unwrap(),
            &signing_key,
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
            signature: Ed25519Signature,
        }

        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let proposal = Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            UncleHeaders::empty(),
            create_proof(),
            BlockTransactions::<RawMantleTx>::empty(),
            &signing_key,
        )
        .unwrap()
        .to_proposal();
        let legacy = LegacyProposal {
            header: proposal.header.clone(),
            uncle_headers: proposal.uncle_headers.clone(),
            references: LegacyReferences {
                mempool_transactions: vec![TxHashPrefix::default(); MAX_BLOCK_TRANSACTIONS + 1],
            },
            signature: *proposal.signature(),
        };
        let bytes = bincode::serialize(&legacy).unwrap();

        let error = <Proposal as crate::codec::DeserializeOp>::from_bytes(&bytes)
            .expect_err("proposal with too many transaction references must be rejected");

        assert!(
            error.to_string().contains("exceeds static maximum"),
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
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);

        let transactions = BlockTransactions::empty();
        let _valid_block: Block<RawMantleTx> = Block::create(
            parent_block,
            slot,
            UncleHeaders::empty(),
            proof_of_leadership.clone(),
            transactions,
            &signing_key,
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
            &signing_key,
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
            &signing_key,
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
        assert_eq!(BlockTransactions::<RawMantleTx>::MIN, 0);
        assert_eq!(
            BlockTransactions::<RawMantleTx>::MAX,
            MAX_BLOCK_TRANSACTIONS
        );
    }

    #[test]
    fn test_create_rejects_genesis_slot() {
        let parent_block = [0u8; 32].into();
        let proof = create_proof();

        // Build a syntactically valid non-genesis block first.
        let txs = BlockTransactions::<RawMantleTx>::empty();
        let key = Ed25519Key::from_bytes(&[0; 32]);
        let block_result = Block::create(
            parent_block,
            Slot::from(0u64),
            UncleHeaders::empty(),
            proof,
            txs,
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
            BlockTransactions::<RawMantleTx>::empty(),
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
            BlockTransactions::<RawMantleTx>::empty(),
            genesis_signature,
        )
        .expect_err("genesis slot must be rejected by reconstruct path");

        assert!(matches!(err, Error::Header(HeaderError::GenesisSlot)));
    }

    /// The specification fixes the maximum proposal at 18,192 bytes:
    /// `header (297) || uncle_headers (1 + MAX_UNCLES * 361)
    /// || references (2 + 16384) || signature (64)`.
    #[test]
    fn maximum_proposal_matches_the_specified_size() {
        use lb_codec::BinaryEncode as _;
        use lb_cryptarchia_engine::MAX_UNCLES;

        const SPECIFIED_MAX_PROPOSAL_SIZE: usize = 18_192;

        let proof = create_proof();
        let uncle = signed_uncle(1, &proof);
        let proposal = Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            UncleHeaders::new(std::array::from_fn::<_, MAX_UNCLES, _>(|_| uncle.clone())),
            proof,
            BlockTransactions::<RawMantleTx>::try_from(create_tx(MAX_BLOCK_TRANSACTIONS)).unwrap(),
            &Ed25519Key::from_bytes(&[0; 32]),
        )
        .expect("valid block")
        .to_proposal();

        assert_eq!(proposal.encoded_length(), SPECIFIED_MAX_PROPOSAL_SIZE);
        assert_eq!(proposal.encode().len(), SPECIFIED_MAX_PROPOSAL_SIZE);
    }

    #[test]
    fn body_root_accepts_carried_uncle_headers() {
        let proof = create_proof();
        let uncles = UncleHeaders::new([signed_uncle(1, &proof), signed_uncle(2, &proof)]);

        block_with_uncles(uncles, proof)
            .into_verified()
            .expect("the carried headers are the ones the body root commits to");
    }

    #[test]
    fn body_root_rejects_dropped_uncle_header() {
        let proof = create_proof();
        let uncles = UncleHeaders::new([signed_uncle(1, &proof)]);
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
        let uncles = UncleHeaders::new([signed_uncle(1, &proof)]);
        let mut block = block_with_uncles(uncles, proof.clone());

        // Same count, but a different header than the one committed to.
        block.uncle_headers = UncleHeaders::new([signed_uncle(2, &proof)]);

        assert!(matches!(
            block.into_verified(),
            Err(Error::BodyRootMismatch)
        ));
    }

    #[test]
    fn body_root_rejects_reordered_uncle_headers() {
        let proof = create_proof();
        let (first, second) = (signed_uncle(1, &proof), signed_uncle(2, &proof));
        let mut block =
            block_with_uncles(UncleHeaders::new([first.clone(), second.clone()]), proof);

        block.uncle_headers = UncleHeaders::new([second, first]);

        assert!(matches!(
            block.into_verified(),
            Err(Error::BodyRootMismatch)
        ));
    }

    #[test]
    fn body_root_rejects_tampered_uncle_signature() {
        let proof = create_proof();
        let uncle = signed_uncle(1, &proof);
        let mut block = block_with_uncles(UncleHeaders::new([uncle.clone()]), proof);

        // Replace only the signature, leaving the header it signs untouched.
        let other_signature = uncle
            .header()
            .sign(&Ed25519Key::from_bytes(&[1; 32]))
            .expect("header signing should succeed");
        block.uncle_headers =
            UncleHeaders::new([SignedHeader::new(uncle.header().clone(), other_signature)]);

        assert!(matches!(
            block.into_verified(),
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

    fn block_with_uncles(
        uncle_headers: UncleHeaders,
        proof: Groth16LeaderProof,
    ) -> Block<RawMantleTx> {
        Block::create(
            [0u8; 32].into(),
            Slot::from(42u64),
            uncle_headers,
            proof,
            BlockTransactions::empty(),
            &Ed25519Key::from_bytes(&[0; 32]),
        )
        .expect("block creation should succeed")
    }
}
