mod deser;
pub mod genesis;

use core::fmt::Debug;

use bytes::Bytes;
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};
use lb_utils::bounded::{BoundedError, BoundedVec};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    codec::{DeserializeOp as _, SerializeOp as _},
    header::{ContentId, Header, HeaderId},
    mantle::{StorageSize, Transaction, TxHash},
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
    #[error("Signature error.")]
    Signature,
    #[error("Block root mismatch: calculated content does not match header")]
    BlockRootMismatch,
    #[error("Signing key does not match the leader key in proof of leadership")]
    KeyMismatch,
    #[error("Validation error: {0}")]
    Validation(String),
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
    #[error("Total storage size {size} exceeds maximum of {max} bytes")]
    ContentTooBig { size: usize, max: usize },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub header: Header,
    pub references: References,
    pub signature: Ed25519Signature,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct References {
    pub mempool_transactions: Vec<TxHash>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(bound(serialize = "Tx: Clone + Serialize"))]
pub struct Block<Tx> {
    header: Header,
    signature: Ed25519Signature,
    transactions: BlockTransactions<Tx>,
}

impl<'de, Tx> Deserialize<'de> for Block<Tx>
where
    Tx: Clone + Eq + Deserialize<'de> + Transaction<Hash = TxHash> + StorageSize,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawBlock<Tx> {
            header: Header,
            signature: Ed25519Signature,
            transactions: BlockTransactions<Tx>,
        }

        let raw = RawBlock::<Tx>::deserialize(deserializer)?;

        Self::reconstruct(raw.header, raw.transactions, raw.signature)
            .map_err(serde::de::Error::custom)
    }
}

impl Proposal {
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub const fn references(&self) -> &References {
        &self.references
    }

    #[must_use]
    pub fn mempool_transactions(&self) -> &[TxHash] {
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
        proof_of_leadership: Groth16LeaderProof,
        transactions: BlockTransactions<Tx>,
        signing_key: &Ed25519Key,
    ) -> Result<Self, Error>
    where
        Tx: Transaction<Hash = TxHash> + StorageSize,
    {
        // 1. Non-genesis blocks only
        if slot == Slot::genesis() {
            return Err(Error::Validation("expected non-genesis slot".to_owned()));
        }

        // 2. Expected leader public key
        let expected_leader_public_key = proof_of_leadership.leader_key();
        if expected_leader_public_key != &signing_key.public_key() {
            return Err(Error::KeyMismatch);
        }

        // 3. Block root & header
        let block_root = Self::calculate_content_id(transactions.as_slice());
        let header = Header::new(parent_block, block_root, slot, proof_of_leadership);

        // 4. Signature over the header
        let signature = header.sign(signing_key)?;

        // 5. New block
        let block = Self {
            header,
            signature,
            transactions,
        };

        // 6. Size is ok
        block.validate_total_transactions_size()?;

        Ok(block)
    }

    pub fn reconstruct(
        header: Header,
        transactions: BlockTransactions<Tx>,
        signature: Ed25519Signature,
    ) -> Result<Self, Error>
    where
        Tx: Transaction<Hash = TxHash> + StorageSize,
    {
        let block = Self {
            header,
            signature,
            transactions,
        };
        let block = block.into_verified()?;

        Ok(block)
    }

    fn into_verified(self) -> Result<Self, Error>
    where
        Tx: Transaction<Hash = TxHash> + StorageSize,
    {
        // 1. Non-genesis blocks only
        if self.header.slot() == Slot::genesis() {
            return Err(Error::Validation("expected non-genesis slot".to_owned()));
        }

        // 2. Size is ok
        self.validate_total_transactions_size()?;

        // 3. Block root matches transactions merkle hash
        let calculated_content_id = Self::calculate_content_id(&self.transactions);
        if self.header.block_root() != &calculated_content_id {
            return Err(Error::BlockRootMismatch);
        }

        // 4. Signature is valid over the header bytes
        let leader_public_key = self.header.leader_proof().leader_key();
        let header_bytes = self.header.to_bytes()?;

        leader_public_key
            .verify(&header_bytes, &self.signature)
            .map_err(|_| Error::Signature)?;

        Ok(self)
    }

    fn validate_total_transactions_size(&self) -> Result<usize, Error>
    where
        Tx: Transaction<Hash = TxHash> + StorageSize,
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

    fn calculate_content_id(transactions: &[Tx]) -> ContentId
    where
        Tx: Transaction<Hash = TxHash>,
    {
        let root_hash = merkle::calculate_block_root(transactions);
        ContentId::from(root_hash)
    }

    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub fn transactions(&self) -> impl ExactSizeIterator<Item = &Tx> + '_ {
        self.transactions.as_slice().iter()
    }

    #[must_use]
    pub fn transactions_vec(&self) -> &Vec<Tx> {
        self.transactions.as_ref()
    }

    #[must_use]
    pub fn into_transactions(self) -> Vec<Tx> {
        self.transactions.into_inner()
    }

    #[must_use]
    pub const fn signature(&self) -> &Ed25519Signature {
        &self.signature
    }

    pub fn to_proposal(self) -> Proposal
    where
        Tx: Transaction<Hash = TxHash>,
    {
        let mempool_transactions: Vec<TxHash> =
            self.transactions.iter().map(Transaction::hash).collect();
        let references = References {
            mempool_transactions,
        };

        Proposal {
            header: self.header,
            references,
            signature: self.signature,
        }
    }
}

impl<Tx: Clone + Eq + Serialize + DeserializeOwned + Transaction<Hash = TxHash> + StorageSize>
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

impl<Tx: Clone + Eq + Serialize + DeserializeOwned> TryFrom<Block<Tx>> for Bytes {
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
            MantleTx, TransactionHasher,
            ledger::{Note, Utxo},
            ops::leader_claim::VoucherCm,
            transactions::Ops,
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

    fn create_tx(count: usize) -> Vec<MantleTx> {
        iter::repeat_with(|| MantleTx(Ops::new_unchecked(vec![])))
            .take(count)
            .collect()
    }

    #[test]
    fn test_block_signature_validation() {
        let parent_block = [0u8; 32].into();
        let slot = Slot::from(42u64);
        let proof_of_leadership = create_proof();
        let transactions = BlockTransactions::<MantleTx>::empty();

        let valid_signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let valid_block = Block::create(
            parent_block,
            slot,
            proof_of_leadership,
            transactions.clone(),
            &valid_signing_key,
        )
        .expect("Valid block should be created");

        let header = valid_block.header().clone();
        let valid_signature = *valid_block.signature();

        let _reconstructed_block =
            Block::reconstruct(header.clone(), transactions.clone(), valid_signature)
                .expect("Should reconstruct block with valid signature");

        let wrong_signing_key = Ed25519Key::from_bytes(&[1u8; 32]);
        let invalid_signature = header
            .sign(&wrong_signing_key)
            .expect("Signing should work");

        let invalid_block_result = Block::reconstruct(header, transactions, invalid_signature);

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
        let _valid_block: Block<MantleTx> = Block::create(
            parent_block,
            slot,
            proof_of_leadership.clone(),
            transactions,
            &signing_key,
        )
        .expect("Valid block should be created");

        let transactions = BlockTransactions::try_from(create_tx(MAX_BLOCK_TRANSACTIONS)).unwrap();
        let _valid_block: Block<MantleTx> = Block::create(
            parent_block,
            slot,
            proof_of_leadership,
            transactions,
            &signing_key,
        )
        .expect("Valid block should be created");

        let invalid_transaction_inputs_result =
            BlockTransactions::<MantleTx>::try_from(create_tx(MAX_BLOCK_TRANSACTIONS + 1));

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

    #[derive(Clone, Copy, Debug)]
    struct TestMantleTx<const SIZE: usize>;

    impl<const SIZE: usize> Transaction for TestMantleTx<SIZE> {
        const HASHER: TransactionHasher<Self> = |_tx| TxHash::from([0u8; 32]);
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
        let _valid_block: Block<MantleTx> = Block::create(
            parent_block,
            slot,
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
        assert_eq!(BlockTransactions::<MantleTx>::MIN, 0);
        assert_eq!(BlockTransactions::<MantleTx>::MAX, MAX_BLOCK_TRANSACTIONS);
    }

    #[test]
    fn test_create_rejects_genesis_slot() {
        let parent_block = [0u8; 32].into();
        let proof = create_proof();

        // Build a syntactically valid non-genesis block first.
        let txs = BlockTransactions::<MantleTx>::empty();
        let key = Ed25519Key::from_bytes(&[0; 32]);
        let block_result =
            Block::create(parent_block, Slot::from(0u64), proof, txs, &key).unwrap_err();

        assert!(
            matches!(block_result, Error::Validation(msg) if msg == "expected non-genesis slot")
        );
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
            proof.clone(),
            BlockTransactions::<MantleTx>::empty(),
            &key,
        )
        .expect("valid non-genesis block");

        // Rebuild header at genesis slot and sign it so signature itself is still
        // consistent.
        let genesis_header = Header::new(
            parent_block,
            *valid.header().block_root(),
            Slot::genesis(),
            proof,
        );
        let genesis_signature = genesis_header
            .sign(&key)
            .expect("header signing should succeed");

        let err = Block::reconstruct(
            genesis_header,
            BlockTransactions::<MantleTx>::empty(),
            genesis_signature,
        )
        .expect_err("genesis slot must be rejected by reconstruct path");

        assert!(matches!(err, Error::Validation(msg) if msg == "expected non-genesis slot"));
    }
}
