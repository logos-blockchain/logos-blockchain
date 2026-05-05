use lb_groth16::Fr;
use nom::IResult;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{OpProof, SignedMantleTx, ops::sdp::SDPDeclareOp};
use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{
        MantleTx, Transaction, TransactionHasher, TxHash,
        encoding::{
            decode_field_element, decode_uint64, decode_unix_timestamp, decode_utf8_string,
            encode_field_element, encode_string, encode_uint64, encode_unix_timestamp,
        },
        gas::{Gas, GasCalculator, GasConstants, GasCost, GasOverflow, GasPrice},
        ops::{
            Op,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
            transfer::TransferOp,
        },
    },
};

/// Initial storage gas price at genesis
///
/// [Spec](https://www.notion.so/nomos-tech/v1-1-Storage-Markets-Specification-326261aa09df804ab483f573f522baf5?source=copy_link#326261aa09df804280b1fd5da1120a14):
/// `P_STR(0)` = 1 LGO/gas
//
// TODO: This is currently set to 0 because zone-sdk and most of e2e tests are
// not paying fees. This must be updated to the correct value defined in the
// spec above.
pub const GENESIS_STORAGE_GAS_PRICE: GasPrice = GasPrice::new(0);

/// Initial execution gas price at genesis
//
// TODO: This is currently set to 0 because zone-sdk and most of e2e tests are
// not paying fees. This must be updated to the correct value once the spec is
// finalized.
pub const GENESIS_EXECUTION_GAS_PRICE: GasPrice = GasPrice::new(0);

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
    UnexpectedInput,
    #[error("Genesis block cannot contain this op: {0:?}")]
    UnsupportedGenesisOp(Vec<Op>),
    #[error(
        "Genesis transaction must have a transfer and an inscription as the two first operations"
    )]
    MissingTransferAndInscription,
    #[error("Invalid genesis inscription: {0:?}")]
    InvalidInscription(Box<Op>),
    #[error("Invalid cryptarchia inscription: {0}")]
    InvalidCryptarchiaParameter(String),
}

impl GenesisTx {
    pub fn from_tx(signed_mantle_tx: SignedMantleTx) -> Result<Self, Error> {
        let mantle_tx = &signed_mantle_tx.mantle_tx;

        // Genesis transactions must have execution gas price and storage gas price
        // matching the expected genesis values
        if mantle_tx.execution_gas_price != GENESIS_EXECUTION_GAS_PRICE
            || mantle_tx.storage_gas_price != GENESIS_STORAGE_GAS_PRICE
        {
            return Err(Error::InvalidGenesisGasPrice);
        }

        // Genesis transactions must contain exactly one transfer as the first op,
        // one inscription as the second op, and then may contain other SDP declarations
        let cryptarchia_parameter = match mantle_tx.ops.as_slice() {
            [
                Op::Transfer(transfer),
                Op::ChannelInscribe(inscription),
                rest @ ..,
            ] => {
                if !transfer.inputs.is_empty() {
                    return Err(Error::UnexpectedInput);
                }
                let cryptarchia_parameter = valid_cryptarchia_inscription(inscription)?;

                let unsupported_ops = rest
                    .iter()
                    .filter(|op| !matches!(op, Op::SDPDeclare(_)))
                    .cloned()
                    .collect::<Vec<_>>();

                if !unsupported_ops.is_empty() {
                    return Err(Error::UnsupportedGenesisOp(unsupported_ops));
                }

                cryptarchia_parameter
            }
            _ => return Err(Error::MissingTransferAndInscription),
        };

        Ok(Self {
            tx: signed_mantle_tx,
            cryptarchia_parameter,
        })
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
    const HASHER: TransactionHasher<Self> = |tx| TxHash(Hasher::digest(tx.as_signing()).into());
    type Hash = TxHash;
    fn as_signing(&self) -> Vec<u8> {
        self.tx.mantle_tx.as_signing()
    }
}

impl GasCalculator for GenesisTx {
    type Context = ();

    fn total_gas_cost<Constants: GasConstants>(
        &self,
        _context: &Self::Context,
    ) -> Result<GasCost, GasOverflow> {
        // Genesis transactions have zero gas cost as per spec
        Ok(0.into())
    }

    fn storage_gas_cost(&self, _context: &Self::Context) -> Result<GasCost, GasOverflow> {
        // Genesis transactions have zero gas cost as per spec
        Ok(0.into())
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        _context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        // Genesis transactions have zero gas cost as per spec
        Ok(0.into())
    }

    fn storage_gas_consumption(&self, _context: &Self::Context) -> Result<Gas, GasOverflow> {
        // Genesis transactions have zero gas cost as per spec
        Ok(0.into())
    }
}

impl crate::mantle::GenesisTx for GenesisTx {
    fn genesis_inscription(&self) -> &InscriptionOp {
        // Safe to unwrap because we validated this in from_tx
        match &self.mantle_tx().ops[1] {
            Op::ChannelInscribe(op) => op,
            _ => unreachable!("GenesisTx always has a valid inscription as second op"),
        }
    }

    fn genesis_transfer(&self) -> &TransferOp {
        // Safe to unwrap because we validated this in from_tx
        match &self.mantle_tx().ops[0] {
            Op::Transfer(op) => op,
            _ => unreachable!("GenesisTx always has a valid transfer as first op"),
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
        #[derive(Deserialize)]
        struct Helper {
            mantle_tx: MantleTx,
            ops_proofs: Vec<OpProof>,
        }

        let helper = Helper::deserialize(deserializer)?;
        let tx = SignedMantleTx::new_unverified(helper.mantle_tx, helper.ops_proofs);
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
        let chain_id = encode_string(&self.chain_id);
        let chain_id_len = u64::try_from(chain_id.len()).expect("chain_id length fits in u64");

        let mut buf = Vec::new();
        buf.extend(encode_uint64(chain_id_len));
        buf.extend(chain_id);
        buf.extend(encode_unix_timestamp(&self.genesis_time));
        buf.extend(encode_field_element(&self.epoch_nonce));
        buf
    }

    /// Decode the inscription from the ad-hoc binary format.
    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        Ok(Self::decode_by_nom(data)
            .map_err(|e| Error::InvalidCryptarchiaParameter(format!("Decoding error: {e}")))?
            .1)
    }

    fn decode_by_nom(data: &[u8]) -> IResult<&[u8], Self> {
        let (data, chain_id_len) = decode_uint64(data)?;
        let (data, chain_id) = decode_utf8_string(data, chain_id_len as usize)?;
        let (data, genesis_time) = decode_unix_timestamp(data)?;
        let (data, epoch_nonce) = decode_field_element(data)?;
        Ok((
            data,
            Self {
                chain_id,
                genesis_time,
                epoch_nonce,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Field as _;
    use lb_key_management_system_keys::keys::{Ed25519Signature, ZkKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{
            ledger::{Inputs, Note, Outputs, Utxo, Value},
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
    fn create_tx(mut ops: Vec<Op>, mut ops_proofs: Vec<OpProof>) -> SignedMantleTx {
        let transfer_op = TransferOp::new(
            Inputs::new(vec![]),
            Outputs::new(vec![create_test_note(1000)]),
        );
        let mut new_ops = vec![Op::Transfer(transfer_op)];
        new_ops.append(&mut ops);
        let mantle_tx = MantleTx {
            ops: new_ops,
            execution_gas_price: GENESIS_EXECUTION_GAS_PRICE,
            storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
        };
        let mut new_op_proofs = vec![OpProof::ZkSig(
            ZkKey::multi_sign(&[], &mantle_tx.hash().to_fr()).unwrap(),
        )];
        new_op_proofs.append(&mut ops_proofs);
        SignedMantleTx {
            mantle_tx,
            ops_proofs: new_op_proofs,
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
            vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
                &[0u8; 64],
            ))],
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
            vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
                &[0u8; 64],
            ))],
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
            vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
                &[0u8; 64],
            ))],
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
            vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
                &[0u8; 64],
            ))],
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
            (vec![], Some(Error::MissingTransferAndInscription)),
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
            let ops_proofs =
                vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(&[0u8; 64])); ops.len()];
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
        let utxo1 = Utxo::new([0u8; 32], 0, create_test_note(1000));
        let utxo2 = Utxo::new([1u8; 32], 1, create_test_note(2000));
        let sdp_declare_op_helper = |utxo_to_use: Utxo, zk_id_value: u8| {
            sdp_declare_op(utxo_to_use, zk_id_value, verifying_key)
        };

        // Test cases: (operations, expected_error)
        let test_cases = [
            // SDP without inscription
            (
                vec![Op::SDPDeclare(sdp_declare_op_helper(utxo1, 0))],
                Some(Error::MissingTransferAndInscription),
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
            let ops_proofs =
                vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(&[0u8; 64])); ops.len()];
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
        // Should succeed with execution_gas_price=GENESIS_EXECUTION_GAS_PRICE
        // and storage_gas_price=GENESIS_STORAGE_GAS_PRICE
        let mut signed_mantle_tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op(
                ChannelId::from([0; 32]),
                &cryptarchia_param(),
                MsgId::root(),
                Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
            ))],
            vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
                &[0u8; 64],
            ))],
        );
        assert!(GenesisTx::from_tx(signed_mantle_tx.clone()).is_ok());

        // Test with wrong execution gas price
        signed_mantle_tx.mantle_tx.execution_gas_price =
            (GENESIS_EXECUTION_GAS_PRICE.into_inner() + 1).into();
        let result = GenesisTx::from_tx(signed_mantle_tx.clone());
        assert_eq!(result, Err(Error::InvalidGenesisGasPrice));

        // Test with wrong storage gas price
        signed_mantle_tx.mantle_tx.storage_gas_price =
            (GENESIS_STORAGE_GAS_PRICE.into_inner() + 1).into();
        signed_mantle_tx.mantle_tx.execution_gas_price = 0.into();
        let result = GenesisTx::from_tx(signed_mantle_tx.clone());
        assert_eq!(result, Err(Error::InvalidGenesisGasPrice));

        // Test with wrong storage/execution gas prices
        signed_mantle_tx.mantle_tx.storage_gas_price =
            (GENESIS_STORAGE_GAS_PRICE.into_inner() + 1).into();
        signed_mantle_tx.mantle_tx.execution_gas_price =
            (GENESIS_EXECUTION_GAS_PRICE.into_inner() + 1).into();
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
            vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
                &[0u8; 64],
            ))],
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
            CryptarchiaParameter::decode(&encoded),
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
            vec![OpProof::Ed25519Sig(Ed25519Signature::zero())],
        );
        let genesis_tx = GenesisTx::from_tx(tx).unwrap();
        assert_eq!(genesis_tx.cryptarchia_parameter(), param);
    }
}
