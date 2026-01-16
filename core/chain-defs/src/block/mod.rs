use core::fmt::Debug;

use ::serde::{Deserialize, Serialize, de::DeserializeOwned};
use bytes::Bytes;
use cryptarchia_engine::Slot;
use key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};

use crate::{
    codec::{DeserializeOp as _, SerializeOp as _},
    header::{ContentId, Header, HeaderId},
    mantle::{Transaction, TxHash},
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof as _},
    utils::merkle,
};

pub const MAX_TRANSACTIONS: usize = 1024;

pub type BlockNumber = u64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to serialize: {0}")]
    Serialisation(#[from] crate::codec::Error),
    #[error("Signature error.")]
    Signature,
    #[error("Too many transactions: {count} exceeds maximum of {max}")]
    TooManyTxs { count: usize, max: usize },
    #[error("Block root mismatch: calculated content does not match header")]
    BlockRootMismatch,
    #[error("Signing key does not match the leader key in proof of leadership")]
    KeyMismatch,
    #[error("Validation error: {0}")]
    Validation(String),
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block<Tx> {
    header: Header,
    signature: Ed25519Signature,
    transactions: Vec<Tx>,
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

impl<Tx> Block<Tx> {
    pub fn create(
        parent_block: HeaderId,
        slot: Slot,
        proof_of_leadership: Groth16LeaderProof,
        transactions: Vec<Tx>,
        signing_key: &Ed25519Key,
    ) -> Result<Self, Error>
    where
        Tx: Transaction<Hash = TxHash>,
    {
        let expected_public_key = proof_of_leadership.leader_key();
        let actual_public_key = signing_key.public_key();
        if expected_public_key != &actual_public_key {
            return Err(Error::KeyMismatch);
        }

        if transactions.len() > MAX_TRANSACTIONS {
            return Err(Error::TooManyTxs {
                count: transactions.len(),
                max: MAX_TRANSACTIONS,
            });
        }

        let block_root = Self::calculate_content_id(&transactions);

        let header = Header::new(parent_block, block_root, slot, proof_of_leadership);

        let signature = header.sign(signing_key)?;

        Ok(Self {
            header,
            signature,
            transactions,
        })
    }

    pub fn reconstruct(
        header: Header,
        transactions: Vec<Tx>,
        signature: Ed25519Signature,
    ) -> Result<Self, Error>
    where
        Tx: Transaction<Hash = TxHash>,
    {
        if transactions.len() > MAX_TRANSACTIONS {
            return Err(Error::TooManyTxs {
                count: transactions.len(),
                max: MAX_TRANSACTIONS,
            });
        }

        let calculated_content_id = Self::calculate_content_id(&transactions);
        if header.block_root() != &calculated_content_id {
            return Err(Error::BlockRootMismatch);
        }

        let leader_public_key = header.leader_proof().leader_key();
        let header_bytes = header.to_bytes()?;

        leader_public_key
            .verify(&header_bytes, &signature)
            .map_err(|_| Error::Signature)?;

        Ok(Self {
            header,
            signature,
            transactions,
        })
    }

    fn calculate_content_id(transactions: &[Tx]) -> ContentId
    where
        Tx: Transaction<Hash = TxHash>,
    {
        let tx_hashes: Vec<TxHash> = transactions.iter().map(Transaction::hash).collect();

        let root_hash = merkle::calculate_merkle_root(&tx_hashes, None);
        ContentId::from(root_hash)
    }

    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub fn transactions(&self) -> impl ExactSizeIterator<Item = &Tx> + '_ {
        self.transactions.iter()
    }

    #[must_use]
    pub const fn transactions_vec(&self) -> &Vec<Tx> {
        &self.transactions
    }

    #[must_use]
    pub fn into_transactions(self) -> Vec<Tx> {
        self.transactions
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

impl<Tx: Clone + Eq + Serialize + DeserializeOwned> TryFrom<Bytes> for Block<Tx> {
    type Error = crate::codec::Error;

    fn try_from(bytes: Bytes) -> Result<Self, Self::Error> {
        Self::from_bytes(&bytes)
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

    use groth16::Fr;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{
            ledger::{Note, Tx, Utxo},
            ops::leader_claim::VoucherCm,
        },
        proofs::leader_proof::{LeaderPrivate, LeaderPublic},
        utils::merkle::MerkleNode,
    };

    pub fn create_proof() -> Groth16LeaderProof {
        let public_inputs = LeaderPublic::new(Fr::from(1), Fr::from(2), Fr::from(3), 0, 1000);

        let utxo = Utxo {
            tx_hash: Fr::from(BigUint::from(1u8)).into(),
            output_index: 0,
            note: Note::new(100, Fr::from(5).into()),
        };

        let aged_path = vec![MerkleNode::Right(Fr::from(0u8))];
        let latest_path = vec![MerkleNode::Left(Fr::from(0u8))];

        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let verifying_key = signing_key.public_key();

        let private_inputs = LeaderPrivate::new(
            public_inputs,
            utxo,
            &aged_path,
            &latest_path,
            Fr::from(6),
            &verifying_key,
        );
        Groth16LeaderProof::prove(private_inputs, VoucherCm::default())
            .expect("Proof generation should succeed")
    }

    fn create_transactions(count: usize) -> Vec<Tx> {
        iter::repeat_with(|| Tx {
            inputs: vec![],
            outputs: vec![],
        })
        .take(count)
        .collect()
    }

    #[test]
    fn test_block_signature_validation() {
        let parent_block = [0u8; 32].into();
        let slot = Slot::from(42u64);
        let proof_of_leadership = create_proof();
        let transactions: Vec<Tx> = vec![];

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

        let _valid_block: Block<Tx> = Block::create(
            parent_block,
            slot,
            proof_of_leadership.clone(),
            vec![],
            &signing_key,
        )
        .expect("Valid block should be created");

        let invalid_block_result = Block::create(
            parent_block,
            slot,
            proof_of_leadership,
            create_transactions(MAX_TRANSACTIONS + 1),
            &signing_key,
        );

        assert!(invalid_block_result.is_err());
        let error = invalid_block_result.unwrap_err();

        let expected_count = MAX_TRANSACTIONS + 1;
        assert!(
            matches!(error, Error::TooManyTxs { count, max } if count == expected_count && max == MAX_TRANSACTIONS)
        );
    }
}
