use std::sync::LazyLock;

use bytes::Bytes;
use groth16::{
    Fr, GROTH16_SAFE_BYTES_SIZE, fr_from_bytes, fr_from_bytes_unchecked, fr_to_bytes,
    serde::serde_fr,
};
use key_management_system_keys::keys::ZkSignature;
use num_bigint::BigUint;
use poseidon2::{Digest, ZkHash};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    crypto::ZkHasher,
    mantle::{
        AuthenticatedMantleTx, Transaction, TransactionHasher,
        encoding::{decode_mantle_tx, encode_mantle_tx, encode_signed_mantle_tx},
        gas::{Gas, GasConstants, GasCost},
        ledger::Tx as LedgerTx,
        ops::{Op, OpProof},
    },
};

/// The hash of a transaction
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TxHash(#[serde(with = "serde_fr")] pub ZkHash);

impl From<ZkHash> for TxHash {
    fn from(fr: ZkHash) -> Self {
        Self(fr)
    }
}

impl From<BigUint> for TxHash {
    fn from(value: BigUint) -> Self {
        Self(value.into())
    }
}

impl From<TxHash> for ZkHash {
    fn from(hash: TxHash) -> Self {
        hash.0
    }
}

impl AsRef<ZkHash> for TxHash {
    fn as_ref(&self) -> &ZkHash {
        &self.0
    }
}

impl From<TxHash> for Bytes {
    fn from(tx_hash: TxHash) -> Self {
        Self::copy_from_slice(&fr_to_bytes(tx_hash.as_ref()))
    }
}

impl From<TxHash> for [u8; 32] {
    fn from(tx_hash: TxHash) -> Self {
        fr_to_bytes(tx_hash.as_ref())
    }
}

impl TxHash {
    /// For testing purposes
    #[cfg(test)]
    pub fn random(mut rng: impl rand::RngCore) -> Self {
        Self(BigUint::from(rng.next_u64()).into())
    }

    #[must_use]
    pub fn as_signing_bytes(&self) -> Bytes {
        self.0.0.0.iter().flat_map(|b| b.to_le_bytes()).collect()
    }
}

#[derive(Serialize, Deserialize)]
struct MantleTxDeSerImpl {
    pub ops: Vec<Op>,
    pub ledger_tx: LedgerTx,
    pub execution_gas_price: Gas,
    pub storage_gas_price: Gas,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MantleTx {
    pub ops: Vec<Op>,
    pub ledger_tx: LedgerTx,
    pub execution_gas_price: Gas,
    pub storage_gas_price: Gas,
}

impl From<MantleTxDeSerImpl> for MantleTx {
    fn from(
        MantleTxDeSerImpl {
            ops,
            ledger_tx,
            execution_gas_price,
            storage_gas_price,
        }: MantleTxDeSerImpl,
    ) -> Self {
        Self {
            ops,
            ledger_tx,
            execution_gas_price,
            storage_gas_price,
        }
    }
}

impl From<MantleTx> for MantleTxDeSerImpl {
    fn from(
        MantleTx {
            ops,
            ledger_tx,
            execution_gas_price,
            storage_gas_price,
        }: MantleTx,
    ) -> Self {
        Self {
            ops,
            ledger_tx,
            execution_gas_price,
            storage_gas_price,
        }
    }
}

impl Serialize for MantleTx {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let tx_deser: MantleTxDeSerImpl = self.clone().into();
            tx_deser.serialize(serializer)
        } else {
            let bytes = encode_mantle_tx(self);
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> Deserialize<'de> for MantleTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            <MantleTxDeSerImpl as Deserialize>::deserialize(deserializer).map(Into::into)
        } else {
            let bytes: Vec<u8> = <Vec<u8>>::deserialize(deserializer)?;
            decode_mantle_tx(&bytes)
                .map(|(_, tx)| tx)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl GasCost for MantleTx {
    fn gas_cost<Constants: GasConstants>(&self) -> Gas {
        let execution_gas = self
            .ops
            .iter()
            .map(Op::execution_gas::<Constants>)
            .sum::<Gas>()
            + self.ledger_tx.execution_gas::<Constants>();
        let storage_gas = self.signed_serialized_size();
        let da_gas_cost = self.ops.iter().map(Op::da_gas_cost).sum::<Gas>();

        execution_gas * self.execution_gas_price
            + storage_gas * self.storage_gas_price
            + da_gas_cost
    }
}

impl MantleTx {
    #[must_use]
    pub fn signed_serialized_size(&self) -> u64 {
        super::encoding::predict_signed_mantle_tx_size(self) as u64
    }
}

static MANTLE_TXHASH_V1_FR: LazyLock<Fr> =
    LazyLock::new(|| fr_from_bytes(b"MANTLE_TXHASH_V1").expect("Constant should be valid Fr"));

impl Transaction for MantleTx {
    const HASHER: TransactionHasher<Self> =
        |tx| <ZkHasher as Digest>::digest(&tx.as_signing_frs()).into();
    type Hash = TxHash;

    fn as_signing_frs(&self) -> Vec<Fr> {
        // constant and structure as defined in the Mantle specification:
        // https://www.notion.so/Mantle-Specification-21c261aa09df810c8820fab1d78b53d9
        let encoded_bytes = encode_mantle_tx(self);
        let frs = encoded_bytes
            .as_slice()
            .chunks(GROTH16_SAFE_BYTES_SIZE)
            // safety: Any 31 bytes fits into a groth16 Fr, there is no need to check for ranges
            .map(fr_from_bytes_unchecked);
        std::iter::once(*MANTLE_TXHASH_V1_FR).chain(frs).collect()
    }
}

impl From<SignedMantleTx> for MantleTx {
    fn from(signed_tx: SignedMantleTx) -> Self {
        signed_tx.mantle_tx
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignedMantleTx {
    pub mantle_tx: MantleTx,
    // TODO: make this more efficient
    pub ops_proofs: Vec<OpProof>,
    pub ledger_tx_proof: ZkSignature,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum VerificationError {
    #[error("Invalid signature for operation at index {op_index}")]
    InvalidSignature { op_index: usize },
    #[error("Missing required proof for {op_type} operation at index {op_index}")]
    MissingProof {
        op_type: &'static str,
        op_index: usize,
    },
    #[error("Incorrect proof type for {op_type} operation at index {op_index}")]
    IncorrectProofType {
        op_type: &'static str,
        op_index: usize,
    },
    #[error("Number of proofs ({proofs_count}) does not match number of operations ({ops_count})")]
    ProofCountMismatch {
        ops_count: usize,
        proofs_count: usize,
    },
}

impl SignedMantleTx {
    /// Create a new `SignedMantleTx` and verify that all required proofs are
    /// present and valid.
    ///
    /// This enforces at construction time that:
    /// - `ChannelBlob` operations have a valid Ed25519 signature from the
    ///   declared signer
    /// - `ChannelInscribe` operations have a valid Ed25519 signature from the
    ///   declared signer
    pub fn new(
        mantle_tx: MantleTx,
        ops_proofs: Vec<OpProof>,
        ledger_tx_proof: ZkSignature,
    ) -> Result<Self, VerificationError> {
        let tx = Self {
            mantle_tx,
            ops_proofs,
            ledger_tx_proof,
        };
        tx.verify_ops_proofs()?;
        Ok(tx)
    }

    /// Create a `SignedMantleTx` without verifying proofs.
    /// This should only be used for `GenesisTx` or in tests.
    #[doc(hidden)]
    #[must_use]
    pub const fn new_unverified(
        mantle_tx: MantleTx,
        ops_proofs: Vec<OpProof>,
        ledger_tx_proof: ZkSignature,
    ) -> Self {
        Self {
            mantle_tx,
            ops_proofs,
            ledger_tx_proof,
        }
    }

    // TODO: might drop proofs after verification
    fn verify_ops_proofs(&self) -> Result<(), VerificationError> {
        // Check that we have the same number of proofs as ops
        if self.mantle_tx.ops.len() != self.ops_proofs.len() {
            return Err(VerificationError::ProofCountMismatch {
                ops_count: self.mantle_tx.ops.len(),
                proofs_count: self.ops_proofs.len(),
            });
        }

        let tx_hash = self.hash();
        let tx_hash_bytes = tx_hash.as_signing_bytes();

        for (idx, (op, proof)) in self
            .mantle_tx
            .ops
            .iter()
            .zip(self.ops_proofs.iter())
            .enumerate()
        {
            match (op, proof) {
                (Op::ChannelBlob(blob_op), OpProof::Ed25519Sig(sig)) => {
                    // Blob operations require an Ed25519 signature
                    blob_op
                        .signer
                        .verify(tx_hash_bytes.as_ref(), sig)
                        .map_err(|_| VerificationError::InvalidSignature { op_index: idx })?;
                }
                (Op::ChannelInscribe(inscribe_op), OpProof::Ed25519Sig(sig)) => {
                    // Inscription operations require an Ed25519 signature
                    inscribe_op
                        .signer
                        .verify(tx_hash_bytes.as_ref(), sig)
                        .map_err(|_| VerificationError::InvalidSignature { op_index: idx })?;
                }
                v @ (Op::ChannelBlob(_) | Op::ChannelInscribe(_), OpProof::ZkSig(_)) => {
                    return Err(VerificationError::IncorrectProofType {
                        op_type: v.0.as_str(),
                        op_index: idx,
                    });
                }
                // Other operations are checked by the ledger or don't require verification here
                _ => {}
            }
        }

        Ok(())
    }

    fn gas_storage_size(&self) -> u64 {
        encode_signed_mantle_tx(self).len() as u64
    }
}

impl Transaction for SignedMantleTx {
    const HASHER: TransactionHasher<Self> =
        |tx| <ZkHasher as Digest>::digest(&tx.as_signing_frs()).into();
    type Hash = TxHash;

    fn as_signing_frs(&self) -> Vec<Fr> {
        self.mantle_tx.as_signing_frs()
    }
}

impl AuthenticatedMantleTx for SignedMantleTx {
    fn mantle_tx(&self) -> &MantleTx {
        &self.mantle_tx
    }

    fn ledger_tx_proof(&self) -> &ZkSignature {
        &self.ledger_tx_proof
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)> {
        self.mantle_tx.ops.iter().zip(self.ops_proofs.iter())
    }
}

impl GasCost for SignedMantleTx {
    fn gas_cost<Constants: GasConstants>(&self) -> Gas {
        let execution_gas = self
            .mantle_tx
            .ops
            .iter()
            .map(Op::execution_gas::<Constants>)
            .sum::<Gas>()
            + self.mantle_tx.ledger_tx.execution_gas::<Constants>();
        let storage_gas = self.gas_storage_size();
        let da_gas_cost = self.mantle_tx.ops.iter().map(Op::da_gas_cost).sum::<Gas>();

        execution_gas * self.mantle_tx.execution_gas_price
            + storage_gas * self.mantle_tx.storage_gas_price
            + da_gas_cost
    }
}

impl<'de> Deserialize<'de> for SignedMantleTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SignedMantleTxHelper {
            mantle_tx: MantleTx,
            ops_proofs: Vec<OpProof>,
            ledger_tx_proof: ZkSignature,
        }

        let helper = SignedMantleTxHelper::deserialize(deserializer)?;
        Self::new(helper.mantle_tx, helper.ops_proofs, helper.ledger_tx_proof)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use key_management_system_keys::keys::{Ed25519Key, ZkKey};

    use super::*;
    use crate::mantle::{
        ledger::Tx as LedgerTx,
        ops::channel::{blob::BlobOp, inscribe::InscriptionOp},
    };

    fn create_test_mantle_tx(ops: Vec<Op>) -> MantleTx {
        MantleTx {
            ops,
            ledger_tx: LedgerTx::new(vec![], vec![]),
            execution_gas_price: 1,
            storage_gas_price: 1,
        }
    }

    fn create_test_blob_op(signing_key: &Ed25519Key) -> BlobOp {
        BlobOp {
            channel: [0; 32].into(),
            session: 0u64,
            blob: [0; 32],
            blob_size: 0,
            da_storage_gas_price: 0,
            parent: [0; 32].into(),
            signer: signing_key.public_key(),
        }
    }

    fn create_test_inscribe_op(signing_key: &Ed25519Key) -> InscriptionOp {
        InscriptionOp {
            channel_id: [0; 32].into(),
            inscription: vec![1, 2, 3],
            parent: [0; 32].into(),
            signer: signing_key.public_key(),
        }
    }

    #[test]
    fn test_signed_mantle_tx_new_with_valid_blob_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let blob_op = create_test_blob_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelBlob(blob_op)]);

        // Sign the transaction hash
        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(signature)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_signed_mantle_tx_new_with_valid_inscribe_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Sign the transaction hash
        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(signature)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_signed_mantle_tx_new_missing_inscribe_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        let ledger_tx_proof = ZkKey::multi_sign(&[], mantle_tx.hash().as_ref()).unwrap();
        let result = SignedMantleTx::new(mantle_tx, vec![], ledger_tx_proof);

        assert!(matches!(
            result,
            Err(VerificationError::ProofCountMismatch {
                ops_count: 1,
                proofs_count: 0
            })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_invalid_blob_signature() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let wrong_signing_key = Ed25519Key::from_bytes(&[2; 32]);
        let blob_op = create_test_blob_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelBlob(blob_op)]);

        // Sign with wrong key
        let tx_hash = mantle_tx.hash();
        let signature = wrong_signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(signature)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        );

        assert!(matches!(
            result,
            Err(VerificationError::InvalidSignature { op_index: 0 })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_invalid_inscribe_signature() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let wrong_signing_key = Ed25519Key::from_bytes(&[2; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Sign with wrong key
        let tx_hash = mantle_tx.hash();
        let signature = wrong_signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(signature)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        );

        assert!(matches!(
            result,
            Err(VerificationError::InvalidSignature { op_index: 0 })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_incorrect_blob_proof_type() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let blob_op = create_test_blob_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelBlob(blob_op)]);

        // Use wrong proof type
        let tx_hash = mantle_tx.hash();
        let zk_sig = OpProof::ZkSig(ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap());
        let ledger_tx_proof = ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap();
        let result = SignedMantleTx::new(mantle_tx, vec![zk_sig], ledger_tx_proof);

        assert!(matches!(
            result,
            Err(VerificationError::IncorrectProofType {
                op_type: "ChannelBlob",
                op_index: 0
            })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_incorrect_inscribe_proof_type() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Use wrong proof type
        let tx_hash = mantle_tx.hash();
        let zk_sig = OpProof::ZkSig(ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap());
        let ledger_tx_proof = ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap();
        let result = SignedMantleTx::new(mantle_tx, vec![zk_sig], ledger_tx_proof);

        assert!(matches!(
            result,
            Err(VerificationError::IncorrectProofType {
                op_type: "ChannelInscribe",
                op_index: 0
            })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_multiple_ops_valid() {
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);

        let blob_op = create_test_blob_op(&signing_key1);
        let inscribe_op = create_test_inscribe_op(&signing_key2);

        let mantle_tx = create_test_mantle_tx(vec![
            Op::ChannelBlob(blob_op),
            Op::ChannelInscribe(inscribe_op),
        ]);

        let tx_hash = mantle_tx.hash();
        let sig1 = signing_key1.sign_payload(&tx_hash.as_signing_bytes());
        let sig2 = signing_key2.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_signed_mantle_tx_new_multiple_ops_one_invalid() {
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[3; 32]);

        let blob_op = create_test_blob_op(&signing_key1);
        let inscribe_op = create_test_inscribe_op(&signing_key2);

        let mantle_tx = create_test_mantle_tx(vec![
            Op::ChannelBlob(blob_op),
            Op::ChannelInscribe(inscribe_op),
        ]);

        let tx_hash = mantle_tx.hash();
        let sig1 = signing_key1.sign_payload(&tx_hash.as_signing_bytes());
        let sig2 = wrong_key.sign_payload(&tx_hash.as_signing_bytes()); // Wrong signature

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        );

        assert!(matches!(
            result,
            Err(VerificationError::InvalidSignature { op_index: 1 })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_with_valid_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let blob_op = create_test_blob_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelBlob(blob_op)]);

        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(signature)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        )
        .unwrap();

        // Serialize and deserialize
        let serialized = serde_json::to_string(&signed_tx).unwrap();
        let deserialized: Result<SignedMantleTx, _> = serde_json::from_str(&serialized);

        assert!(deserialized.is_ok());
        assert_eq!(deserialized.unwrap(), signed_tx);
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_with_missing_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let blob_op = create_test_blob_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelBlob(blob_op)]);

        let ledger_tx_proof = ZkKey::multi_sign(&[], mantle_tx.hash().as_ref()).unwrap();
        let helper = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![],
            ledger_tx_proof,
        };

        let serialized = serde_json::to_string(&helper).unwrap();
        let deserialized: Result<SignedMantleTx, _> = serde_json::from_str(&serialized);

        assert!(deserialized.is_err());
        let err_msg = deserialized.unwrap_err().to_string();
        assert_eq!(
            err_msg,
            "Number of proofs (0) does not match number of operations (1)"
        );
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_with_invalid_signature() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[2; 32]);
        let blob_op = create_test_blob_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelBlob(blob_op)]);

        let tx_hash = mantle_tx.hash();
        let wrong_signature = wrong_key.sign_payload(&tx_hash.as_signing_bytes());

        let helper = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![OpProof::Ed25519Sig(wrong_signature)],
            ledger_tx_proof: ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        };

        let serialized = serde_json::to_string(&helper).unwrap();
        let deserialized: Result<SignedMantleTx, _> = serde_json::from_str(&serialized);

        assert!(deserialized.is_err());
        let err_msg = deserialized.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid signature"));
    }

    #[test]
    fn test_signed_mantle_tx_new_proof_count_mismatch() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let blob_op = create_test_blob_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelBlob(blob_op)]);
        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        // Test too few proofs
        let ledger_tx_proof = ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap();
        let result = SignedMantleTx::new(mantle_tx.clone(), vec![], ledger_tx_proof);
        assert!(matches!(
            result,
            Err(VerificationError::ProofCountMismatch {
                ops_count: 1,
                proofs_count: 0
            })
        ));

        // Test too many proofs
        let result = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(signature),
                OpProof::Ed25519Sig(signature),
            ],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        );
        assert!(matches!(
            result,
            Err(VerificationError::ProofCountMismatch {
                ops_count: 1,
                proofs_count: 2
            })
        ));
    }
}
