use lb_groth16::{Fr, FrBytes, fr_from_bytes, fr_to_bytes};
use lb_poseidon2::Digest;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{OpProof, SignedMantleTx, ops::sdp::SDPDeclareOp};
use crate::{
    crypto::ZkHasher,
    mantle::{
        MantleTx, Transaction, TransactionHasher, TxHash,
        gas::{Gas, GasConstants, GasCost},
        ops::{
            Op,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
        },
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisTx {
    tx: SignedMantleTx,
    cryptarchia_parameter: CryptarchiaParameter,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Genesis transaction must have gas price of zero")]
    InvalidGenesisGasPrice,
    #[error("Genesis transaction should not have any inputs")]
    UnepectedInput,
    #[error("Genesis block cannot contain this op: {0:?}")]
    UnsupportedGenesisOp(Vec<Op>),
    #[error("Expected exactly one inscription in genesis block")]
    MissingInscription,
    #[error("Invalid genesis inscription: {0:?}")]
    InvalidInscription(Box<Op>),
    #[error("Invalid cryptarchia inscription: {0}")]
    InvalidCryptarchiaParameter(String),
}

impl GenesisTx {
    pub fn from_tx(signed_mantle_tx: SignedMantleTx) -> Result<Self, Error> {
        let mantle_tx = &signed_mantle_tx.mantle_tx;

        // Genesis transactions must have gas prices of zero
        if mantle_tx.execution_gas_price != 0 || mantle_tx.storage_gas_price != 0 {
            return Err(Error::InvalidGenesisGasPrice);
        }

        // Genesis transactions should not have any inputs
        if !mantle_tx.ledger_tx.inputs.is_empty() {
            return Err(Error::UnepectedInput);
        }

        // Genesis transactions must contain exactly one inscription as the first op
        // and then may contain other SDP declarations
        let mut ops = mantle_tx.ops.iter();
        let cryptarchia_parameter = match ops.next() {
            Some(Op::ChannelInscribe(op)) => valid_cryptarchia_inscription(op)?,
            _ => return Err(Error::MissingInscription),
        };

        let unsupported_ops = ops
            .filter(|op| !matches!(op, Op::SDPDeclare(_)))
            .cloned()
            .collect::<Vec<_>>();
        if !unsupported_ops.is_empty() {
            return Err(Error::UnsupportedGenesisOp(unsupported_ops));
        }

        Ok(Self {
            tx: signed_mantle_tx,
            cryptarchia_parameter,
        })
    }

    #[cfg(feature = "mock")]
    #[must_use]
    pub fn new_mocked() -> Self {
        use lb_groth16::{CompressedGroth16Proof, Field as _};
        use lb_key_management_system_keys::keys::ZkSignature;

        use crate::mantle::{ops::channel::Ed25519PublicKey, tx_builder::MantleTxBuilder};

        let cryptarchia_parameter = CryptarchiaParameter {
            chain_id: "mock-chain-id".to_owned(),
            genesis_time: OffsetDateTime::now_utc(),
            epoch_nonce: Fr::ZERO,
        };
        let inscription_op = InscriptionOp {
            channel_id: ChannelId::from([0; 32]),
            inscription: cryptarchia_parameter.encode(),
            parent: MsgId::root(),
            signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
        };
        Self {
            tx: SignedMantleTx::new_unverified(
                MantleTxBuilder::new()
                    .push_op(Op::ChannelInscribe(inscription_op))
                    .build(),
                vec![OpProof::NoProof],
                ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0; _])),
            ),
            cryptarchia_parameter,
        }
    }
}

fn valid_cryptarchia_inscription(
    inscription: &InscriptionOp,
) -> Result<CryptarchiaParameter, Error> {
    if inscription.parent != MsgId::root() {
        return Err(Error::InvalidInscription(Box::new(Op::ChannelInscribe(
            inscription.clone(),
        ))));
    }

    if inscription.channel_id != ChannelId::from([0; 32]) {
        return Err(Error::InvalidInscription(Box::new(Op::ChannelInscribe(
            inscription.clone(),
        ))));
    }

    if inscription.signer.as_bytes() != &[0; 32] {
        return Err(Error::InvalidInscription(Box::new(Op::ChannelInscribe(
            inscription.clone(),
        ))));
    }

    CryptarchiaParameter::decode(&inscription.inscription)
}

impl Transaction for GenesisTx {
    const HASHER: TransactionHasher<Self> =
        |tx| <ZkHasher as Digest>::digest(&tx.as_signing_frs()).into();
    type Hash = TxHash;
    fn as_signing_frs(&self) -> Vec<Fr> {
        self.tx.mantle_tx.as_signing_frs()
    }
}

impl GasCost for GenesisTx {
    fn gas_cost<Constants: GasConstants>(&self) -> Gas {
        // Genesis transactions have zero gas cost as per spec
        0
    }
}

impl crate::mantle::GenesisTx for GenesisTx {
    fn genesis_inscription(&self) -> &InscriptionOp {
        // Safe to unwrap because we validated this in from_tx
        match &self.mantle_tx().ops[0] {
            Op::ChannelInscribe(op) => op,
            _ => unreachable!("GenesisTx always has a valid inscription as first op"),
        }
    }

    fn cryptarchia_parameter(&self) -> CryptarchiaParameter {
        self.cryptarchia_parameter.clone()
    }

    fn sdp_declarations(&self) -> impl Iterator<Item = (&SDPDeclareOp, &OpProof)> {
        self.mantle_tx()
            .ops
            .iter()
            .zip(self.tx.ops_proofs.iter())
            .filter_map(|(op, proof)| {
                if let Op::SDPDeclare(sdp_msg) = op {
                    Some((sdp_msg, proof))
                } else {
                    None
                }
            })
    }

    fn mantle_tx(&self) -> &MantleTx {
        &self.tx.mantle_tx
    }
}

impl Serialize for GenesisTx {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Skip self.cryptarchia_parameter as it is parsed from the inscription op
        self.tx.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GenesisTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let tx = SignedMantleTx::deserialize(deserializer)?;
        Self::from_tx(tx).map_err(serde::de::Error::custom)
    }
}

/// Cryptarchia parameters encoded as an inscription in the genesis block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptarchiaParameter {
    pub chain_id: String,
    pub genesis_time: OffsetDateTime,
    pub epoch_nonce: Fr,
}

impl CryptarchiaParameter {
    /// Encode the inscription into the deterministic ad-hoc binary format.
    ///
    /// Ad-hoc encoding format:
    /// [u64-chain-id-bytes-len][utf8-encoded-chain-id][u64-genesis-time-as-unix-timestamp-in-seconds][256bit-epoch-nonce]
    ///
    /// All integers are little-endian. The epoch nonce is 32 raw bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let chain_id = self.chain_id.as_bytes();
        let chain_id_len = u64::try_from(chain_id.len())
            .expect("chain_id length fits in u64")
            .to_le_bytes();
        let genesis_time = u64::try_from(self.genesis_time.unix_timestamp())
            .expect("genesis_time fits in u64")
            .to_le_bytes();
        let epoch_nonce = fr_to_bytes(&self.epoch_nonce);

        let mut buf = Vec::with_capacity(
            chain_id_len.len() + chain_id.len() + genesis_time.len() + epoch_nonce.len(),
        );
        buf.extend_from_slice(&chain_id_len);
        buf.extend_from_slice(chain_id);
        buf.extend_from_slice(&genesis_time);
        buf.extend_from_slice(&epoch_nonce);
        buf
    }

    /// Decode the inscription from the ad-hoc binary format.
    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        let chain_id_len = u64::from_le_bytes(
            data.get(..8)
                .ok_or_else(|| Error::InvalidCryptarchiaParameter("inscription too short".into()))?
                .try_into()
                .expect("8-bytes fits in u64"),
        ) as usize;

        let chain_id = data
            .get(8..8 + chain_id_len)
            .ok_or_else(|| Error::InvalidCryptarchiaParameter("inscription too short".into()))?;
        let chain_id = String::from_utf8(chain_id.to_vec()).map_err(|e| {
            Error::InvalidCryptarchiaParameter(format!("invalid chain_id utf8: {e}"))
        })?;

        let genesis_time_offset = 8 + chain_id_len;
        let genesis_time = u64::from_le_bytes(
            data.get(genesis_time_offset..genesis_time_offset + 8)
                .ok_or_else(|| Error::InvalidCryptarchiaParameter("inscription too short".into()))?
                .try_into()
                .expect("8-bytes fits in u64"),
        );
        let genesis_time =
            OffsetDateTime::from_unix_timestamp(genesis_time.try_into().map_err(|e| {
                Error::InvalidCryptarchiaParameter(format!("genesis_time out of range: {e}"))
            })?)
            .map_err(|e| {
                Error::InvalidCryptarchiaParameter(format!("invalid genesis_time: {e}"))
            })?;

        let nonce_offset = genesis_time_offset + 8;
        let epoch_nonce = fr_from_bytes(
            data.get(nonce_offset..nonce_offset + size_of::<FrBytes>())
                .ok_or_else(|| {
                    Error::InvalidCryptarchiaParameter("inscription too short".into())
                })?,
        )
        .map_err(|e| Error::InvalidCryptarchiaParameter(format!("invalid epoch_nonce: {e}")))?;

        Ok(Self {
            chain_id,
            genesis_time,
            epoch_nonce,
        })
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Field as _;
    use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{
            ledger::{Note, Tx as LedgerTx, Utxo, Value},
            ops::channel::Ed25519PublicKey,
        },
        sdp::{ProviderId, ServiceType},
    };

    fn inscription_op(
        channel_id: ChannelId,
        cryptarchia_param: &CryptarchiaParameter,
        parent: MsgId,
        signer: Ed25519PublicKey,
    ) -> InscriptionOp {
        InscriptionOp {
            channel_id,
            inscription: cryptarchia_param.encode(),
            parent,
            signer,
        }
    }

    fn cryptarchia_param() -> CryptarchiaParameter {
        CryptarchiaParameter {
            chain_id: "test".to_owned(),
            genesis_time: OffsetDateTime::from_unix_timestamp(1000).unwrap(),
            epoch_nonce: Fr::ZERO,
        }
    }

    fn sdp_declare_op(
        utxo_to_use: Utxo,
        zk_id_value: u8,
        verifying_key: Ed25519PublicKey,
    ) -> SDPDeclareOp {
        SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: utxo_to_use.id(),
            zk_id: ZkPublicKey::new(BigUint::from(zk_id_value).into()),
            provider_id: ProviderId(verifying_key),
            locators: [].into(),
        }
    }

    // Helper function to create a test note
    fn create_test_note(value: Value) -> Note {
        Note::new(value, ZkPublicKey::from(BigUint::from(123u64)))
    }

    // Helper function to create a basic signed transaction
    // Genesis transactions don't need verified proofs for Blob/Inscription ops
    fn create_tx(ops: Vec<Op>, ops_proofs: Vec<OpProof>) -> SignedMantleTx {
        let ledger_tx = LedgerTx::new(vec![], vec![create_test_note(1000)]);
        let mantle_tx = MantleTx {
            ops,
            ledger_tx,
            execution_gas_price: 0,
            storage_gas_price: 0,
        };
        SignedMantleTx {
            mantle_tx: mantle_tx.clone(),
            ops_proofs,
            ledger_tx_proof: ZkKey::multi_sign(&[], mantle_tx.hash().as_ref()).unwrap(),
        }
    }

    #[test]
    fn test_inscription_fields() {
        // check inscription with channel id [1; 32] fails
        let tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([1; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            ))],
            vec![OpProof::NoProof],
        );
        assert!(matches!(
            GenesisTx::from_tx(tx),
            Err(Error::InvalidInscription(_))
        ));

        // check inscription with non-root parent fails
        let tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::from([1; 32]),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            ))],
            vec![OpProof::NoProof],
        );
        assert!(matches!(
            GenesisTx::from_tx(tx),
            Err(Error::InvalidInscription(_))
        ));

        // check inscription with non-zero signer fails
        let tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[1; 32]).unwrap(),
            ))],
            vec![OpProof::NoProof],
        );
        assert!(matches!(
            GenesisTx::from_tx(tx),
            Err(Error::InvalidInscription(_))
        ));

        // check valid inscription passes
        let tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            ))],
            vec![OpProof::NoProof],
        );
        assert!(GenesisTx::from_tx(tx).is_ok());
    }

    #[test]
    fn test_genesis_inscription_ops() {
        let inscription_op = || {
            inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            )
        };

        // Test cases: (operations, expected_error)
        let test_cases = [
            // no inscription -> error
            (vec![], Some(Error::MissingInscription)),
            // one inscription -> ok
            (vec![Op::ChannelInscribe(inscription_op())], None),
            // two inscriptions -> error
            (
                vec![
                    Op::ChannelInscribe(inscription_op()),
                    Op::ChannelInscribe(inscription_op()),
                ],
                Some(Error::UnsupportedGenesisOp(vec![Op::ChannelInscribe(
                    inscription_op(),
                )])),
            ),
        ];

        // Execute all test cases
        for (ops, expected_err) in test_cases {
            let ops_proofs = vec![OpProof::NoProof; ops.len()];
            let tx = create_tx(ops, ops_proofs);
            let result = GenesisTx::from_tx(tx);
            match expected_err {
                Some(expected) => assert_eq!(result, Err(expected)),
                None => assert!(result.is_ok()),
            }
        }
    }

    #[test]
    fn test_genesis_sdp_ops() {
        let inscription_op = || {
            inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            )
        };
        let verifying_key = Ed25519PublicKey::from_bytes(&[0; 32]).unwrap();
        let utxo1 = Utxo::new(TxHash::from(Fr::from(0u64)), 0, create_test_note(1000));
        let utxo2 = Utxo::new(TxHash::from(Fr::from(1u64)), 1, create_test_note(2000));
        let sdp_declare_op_helper = |utxo_to_use: Utxo, zk_id_value: u8| {
            sdp_declare_op(utxo_to_use, zk_id_value, verifying_key)
        };

        // Test cases: (operations, expected_error)
        let test_cases = [
            // SDP without inscription
            (
                vec![Op::SDPDeclare(sdp_declare_op_helper(utxo1, 0))],
                Some(Error::MissingInscription),
            ),
            // Valid SDP combinations
            (
                vec![
                    Op::ChannelInscribe(inscription_op()),
                    Op::SDPDeclare(sdp_declare_op_helper(utxo1, 0)),
                ],
                None,
            ),
            (
                vec![
                    Op::ChannelInscribe(inscription_op()),
                    Op::SDPDeclare(sdp_declare_op_helper(utxo1, 0)),
                    Op::SDPDeclare(sdp_declare_op_helper(utxo2, 1)),
                ],
                None,
            ),
        ];

        // Execute all test cases
        for (ops, expected_err) in test_cases {
            let ops_proofs = vec![OpProof::NoProof; ops.len()];
            let tx = create_tx(ops, ops_proofs);
            let result = GenesisTx::from_tx(tx);
            match expected_err {
                Some(expected) => assert_eq!(result, Err(expected)),
                None => assert!(result.is_ok()),
            }
        }
    }

    #[test]
    fn test_genesis_fees() {
        // Should succeed with zero gas prices
        let mut signed_mantle_tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            ))],
            vec![OpProof::NoProof],
        );
        assert!(GenesisTx::from_tx(signed_mantle_tx.clone()).is_ok());

        // Test with non-zero execution gas price
        signed_mantle_tx.mantle_tx.execution_gas_price = 1;
        let result = GenesisTx::from_tx(signed_mantle_tx.clone());
        assert_eq!(result, Err(Error::InvalidGenesisGasPrice));

        // test with non-zero storage gas price
        signed_mantle_tx.mantle_tx.storage_gas_price = 1;
        signed_mantle_tx.mantle_tx.execution_gas_price = 0;
        let result = GenesisTx::from_tx(signed_mantle_tx.clone());
        assert_eq!(result, Err(Error::InvalidGenesisGasPrice));

        // test with both gas prices non-zero
        signed_mantle_tx.mantle_tx.storage_gas_price = 1;
        signed_mantle_tx.mantle_tx.execution_gas_price = 1;
        let result = GenesisTx::from_tx(signed_mantle_tx);
        assert_eq!(result, Err(Error::InvalidGenesisGasPrice));
    }

    #[test]
    fn test_genesis_tx_serde() {
        // Create a genesis transaction with inscription (no signature proof required)
        let signed_mantle_tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            ))],
            vec![OpProof::NoProof],
        );
        let genesis_tx = GenesisTx::from_tx(signed_mantle_tx).expect("Valid genesis transaction");

        // Serialize to JSON
        let json_str = serde_json::to_string(&genesis_tx).expect("Serialization should succeed");

        // Deserialize from JSON
        let deserialized: GenesisTx = serde_json::from_str(&json_str).unwrap();

        // Verify they're equal
        assert_eq!(genesis_tx, deserialized);
    }

    #[test]
    fn test_cryptarchia_parameter_roundtrip() {
        let param = cryptarchia_param();
        let encoded = param.encode();
        let decoded = CryptarchiaParameter::decode(&encoded).unwrap();
        assert_eq!(param, decoded);
    }

    #[test]
    fn test_cryptarchia_parameter_decode_errors() {
        // Too short
        assert!(matches!(
            CryptarchiaParameter::decode(&[0; 1]),
            Err(Error::InvalidCryptarchiaParameter(_))
        ));

        // Wrong length (chain_id_len says 100 but only a few bytes follow)
        let mut bad = vec![0; 48];
        bad[0] = 100; // chain_id_len = 100
        assert!(matches!(
            CryptarchiaParameter::decode(&bad),
            Err(Error::InvalidCryptarchiaParameter(_))
        ));

        // Invalid UTF-8 chain_id
        let mut encoded = cryptarchia_param().encode();
        encoded[8] = 0xFF; // corrupt the UTF-8 byte
        assert!(matches!(
            CryptarchiaParameter::decode(&bad),
            Err(Error::InvalidCryptarchiaParameter(_))
        ));
    }

    #[test]
    fn test_genesis_tx_cryptarchia_parameter() {
        use crate::mantle::GenesisTx as _;

        let param = cryptarchia_param();
        let tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([0; 32]),
                &param,
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            ))],
            vec![OpProof::NoProof],
        );
        let genesis_tx = GenesisTx::from_tx(tx).unwrap();
        assert_eq!(genesis_tx.cryptarchia_parameter(), param);
    }
}
