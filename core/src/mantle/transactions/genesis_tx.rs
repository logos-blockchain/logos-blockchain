use core::fmt::{self, Display, Formatter};

use lb_codec::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::Fr;
use lb_utils::bounded::{BoundedString, BoundedVec};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{
        gas::{Gas, GasCost, GasOverflow, GasProfile, TxGasCalculator},
        ledger::verification_mode::GenesisMode,
        ops::{
            Op, OpRef, SignedOp, SignedOperation,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
            sdp::SDPDeclareOp,
            transfer::TransferOp,
        },
        traits::{GenesisTx as GenesisTxTrait, Hashable, MantleTx, genesis::GenesisOps, hashable},
        transactions::{
            OpRefs, SignedOps,
            hash::TxHash,
            states::{Preverified, Unverified},
        },
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisTx {
    signed_ops: SignedOps<Preverified, GenesisMode>,
    cryptarchia_parameter: CryptarchiaParameter,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Genesis transaction must have gas price of zero")]
    InvalidGenesisGasPrice,
    #[error("Genesis transaction should not have any inputs")]
    UnexpectedInput,
    #[error("Genesis block cannot contain this op: {0:?}")]
    UnsupportedGenesisOp(Vec<(usize, &'static str)>),
    #[error(
        "Genesis transaction must have a transfer and an inscription as the two first operations"
    )]
    MissingTransferAndInscription,
    #[error("Invalid genesis inscription: {0:?}")]
    InvalidInscription(Box<Op>), // TODO: Consider adding explanation
    #[error("Invalid cryptarchia inscription: {0}")]
    InvalidCryptarchiaParameter(String),
    #[error("Too many operations in genesis transaction: {count}")]
    TooManyOps { count: usize },
}

impl GenesisTx {
    pub fn from_tx(signed_ops: SignedOps<Preverified, GenesisMode>) -> Result<Self, Error> {
        // Genesis transactions must contain exactly one transfer as the first op,
        // one inscription as the second op, and then may contain other SDP declarations
        let cryptarchia_parameter = match signed_ops.op_refs().as_slice() {
            [
                OpRef::Transfer(transfer),
                OpRef::ChannelInscribe(inscription),
                rest @ ..,
            ] => {
                if !transfer.inputs.is_empty() {
                    return Err(Error::UnexpectedInput);
                }
                let cryptarchia_parameter = valid_cryptarchia_inscription(inscription)?;

                let unsupported_ops = rest
                    .iter()
                    .enumerate()
                    .filter(|(_, op)| !matches!(op, OpRef::SDPDeclare(_)))
                    .map(|(index, op)| (index + 2, op.as_str())) // The `+ 2` accounts for the first two ops (transfer and inscription)
                    .collect::<Vec<_>>();

                if !unsupported_ops.is_empty() {
                    return Err(Error::UnsupportedGenesisOp(unsupported_ops));
                }

                cryptarchia_parameter
            }
            _ => return Err(Error::MissingTransferAndInscription),
        };

        Ok(Self {
            signed_ops,
            cryptarchia_parameter,
        })
    }

    #[must_use]
    pub fn transfer(&self) -> &SignedOperation<TransferOp, Preverified, GenesisMode> {
        match self.signed_ops.first() {
            Some(SignedOp::Transfer(signed_operation)) => signed_operation,
            _ => unreachable!("GenesisTx always has a valid transfer as first op"),
        }
    }

    #[must_use]
    pub fn inscription(&self) -> &SignedOperation<InscriptionOp, Preverified, GenesisMode> {
        match self.signed_ops.get(1) {
            Some(SignedOp::ChannelInscribe(signed_operation)) => signed_operation,
            _ => unreachable!("GenesisTx always has a valid inscription as second op"),
        }
    }

    pub fn sdp_declarations(
        &self,
    ) -> impl Iterator<Item = &SignedOperation<SDPDeclareOp, Preverified, GenesisMode>> {
        self.signed_ops.inner().iter().skip(2).filter_map(|op| {
            if let SignedOp::SDPDeclare(signed_operation) = op {
                Some(signed_operation)
            } else {
                None
            }
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

    Ok(
        CryptarchiaParameter::decode(inscription.inscription.as_ref(), &())
            .map_err(|e| Error::InvalidCryptarchiaParameter(format!("Decoding error: {e}")))?
            .1,
    )
}

impl Hashable for GenesisTx {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> = |tx| TxHash(Hasher::digest(tx.as_signing()).into());
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        self.signed_ops.as_signing()
    }
}

impl TxGasCalculator for GenesisTx {
    type Context = ();

    fn total_gas_cost<Profile: GasProfile>(
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

    fn execution_gas_consumption<Profile: GasProfile>(
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

impl MantleTx for GenesisTx {
    fn op_refs(&self) -> OpRefs<'_> {
        self.signed_ops.op_refs()
    }
}

impl GenesisTxTrait for GenesisTx {
    fn into_genesis_ops(self) -> GenesisOps {
        let mut signed_ops = self.signed_ops.into_inner().into_inner().into_iter();

        let transfer = match signed_ops.next() {
            Some(SignedOp::Transfer(signed_operation)) => signed_operation,
            _ => unreachable!("GenesisTx always has a valid transfer as first op"),
        }
        .into_state_trusted();

        let inscription = match signed_ops.next() {
            Some(SignedOp::ChannelInscribe(signed_operation)) => signed_operation,
            _ => unreachable!("GenesisTx always has a valid inscription as second op"),
        }
        .into_state_trusted();

        let declarations = signed_ops
            .filter_map(|signed_op| {
                if let SignedOp::SDPDeclare(signed_operation) = signed_op {
                    Some(signed_operation)
                } else {
                    None
                }
            })
            .collect();

        GenesisOps {
            transfer,
            inscription,
            declarations,
        }
    }

    fn cryptarchia_parameter(&self) -> CryptarchiaParameter {
        self.cryptarchia_parameter.clone()
    }
}

impl Serialize for GenesisTx {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Skip self.cryptarchia_parameter as it is parsed from the inscription op
        self.signed_ops.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GenesisTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let tx = SignedOps::<Unverified, GenesisMode>::deserialize(deserializer)?
            .into_preverified_trusted();
        Self::from_tx(tx).map_err(serde::de::Error::custom)
    }
}

pub const MAX_CHAIN_ID_SIZE: usize = u8::MAX as usize;
type ChainIdBoundedVec = BoundedVec<u8, 1, MAX_CHAIN_ID_SIZE>;
/// A chain ID is a UTF-8 string whose byte length is bounded to
/// `[1, MAX_CHAIN_ID_SIZE]`. The length invariant is owned by [`Bounded`];
/// the UTF-8 and domain rules live on [`ChainId`] below.
type ChainIdBoundedString = BoundedString<1, MAX_CHAIN_ID_SIZE>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChainId(ChainIdBoundedString);

impl ChainId {
    #[must_use]
    pub const fn new_unchecked(addr: String) -> Self {
        Self(BoundedString::new_unchecked(addr))
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0.into_inner()
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.as_inner().len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.as_inner().is_empty()
    }
}

impl Display for ChainId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for ChainId {
    fn as_ref(&self) -> &str {
        self.0.as_inner().as_str()
    }
}

impl AsRef<[u8]> for ChainId {
    fn as_ref(&self) -> &[u8] {
        self.0.as_inner().as_bytes()
    }
}

impl TryFrom<String> for ChainId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let bounded_chain_id =
            ChainIdBoundedString::try_from(value).map_err(|e| format!("Invalid chain ID: {e}"))?;

        Ok(Self(bounded_chain_id))
    }
}

impl TryFrom<Vec<u8>> for ChainId {
    type Error = String;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        let string = String::from_utf8(value).map_err(|e| format!("Invalid UTF-8 string: {e}"))?;
        Self::try_from(string)
    }
}

impl<const MIN: usize, const MAX: usize> TryFrom<BoundedVec<u8, MIN, MAX>> for ChainId {
    type Error = String;

    fn try_from(value: BoundedVec<u8, MIN, MAX>) -> Result<Self, Self::Error> {
        const {
            assert!(MIN >= 1, "Min size cannot be less than 1.");
            assert!(
                MAX <= MAX_CHAIN_ID_SIZE,
                "Max size cannot be greater than MAX_CHAIN_ID_SIZE."
            );
        }
        Self::try_from(value.into_inner())
    }
}

impl BinaryEncode for ChainId {
    fn encoded_length(&self) -> usize {
        self.0.to_vec().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.to_vec().encode_into(out);
    }
}

impl BinaryDecode for ChainId {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, value) = ChainIdBoundedVec::decode(input, &())?;
        let chain_id = Self::try_from(value)
            .map_err(|_| DecodeError::invalid_value::<Self>("invalid chain id bytes"))?;
        Ok((rest, chain_id))
    }
}

/// Time at which the chain should start. u32 suffices: we only need the
/// positive half of the i64 Unix timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, BinaryCodec)]
pub struct GenesisTime(u32);

impl GenesisTime {
    #[must_use]
    pub const fn new(seconds_since_epoch: u32) -> Self {
        Self(seconds_since_epoch)
    }
}

impl From<GenesisTime> for OffsetDateTime {
    fn from(value: GenesisTime) -> Self {
        Self::from_unix_timestamp(i64::from(value.0))
            .expect("u32 unix timestamp is always a valid OffsetDateTime")
    }
}

impl TryFrom<OffsetDateTime> for GenesisTime {
    type Error = String;
    fn try_from(dt: OffsetDateTime) -> Result<Self, Self::Error> {
        u32::try_from(dt.unix_timestamp())
            .map(Self)
            .map_err(|_| format!("datetime {dt:?} does not fit in u32"))
    }
}

/// Cryptarchia parameters encoded as an inscription in the genesis block.
#[derive(Debug, Clone, PartialEq, Eq, BinaryCodec)]
pub struct CryptarchiaParameter {
    pub chain_id: ChainId,
    pub genesis_time: GenesisTime,
    pub epoch_nonce: Fr,
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, CompressedGroth16Proof};
    use lb_key_management_system_keys::keys::{Ed25519Signature, ZkKey, ZkPublicKey, ZkSignature};
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        mantle::{
            OpProof,
            ledger::{Inputs, Note, Outputs, Utxo, Value},
            ops::{
                ZkAndEd25519Proof,
                channel::{Ed25519PublicKey, inscribe::Inscription},
            },
            transactions::{OpProofs, Ops},
        },
        sdp::{Locator, ProviderId, ServiceType},
    };

    fn inscription_op(
        channel_id: ChannelId,
        cryptarchia_param: &CryptarchiaParameter,
        parent: MsgId,
        signer: Ed25519PublicKey,
    ) -> InscriptionOp {
        InscriptionOp {
            channel_id,
            inscription: Inscription::new_unchecked(cryptarchia_param.encode_to_vec()),
            parent,
            signer,
        }
    }

    fn cryptarchia_param() -> CryptarchiaParameter {
        CryptarchiaParameter {
            chain_id: "test".to_owned().try_into().unwrap(),
            genesis_time: GenesisTime::new(1000),
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
            service_note_id: utxo_to_use.id(),
            zk_id: ZkPublicKey::new(BigUint::from(zk_id_value).into()),
            provider_id: ProviderId(verifying_key),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        }
    }

    // Helper function to create a test note
    fn create_test_note(value: Value) -> Note {
        Note::new(value, ZkPublicKey::from(BigUint::from(123u64)))
    }

    // Helper function to build a proof of the variant expected for a given op
    fn placeholder_proof(op: &Op) -> OpProof {
        match op {
            Op::ChannelInscribe(_) => OpProof::Ed25519Sig(Ed25519Signature::zero()),
            Op::Transfer(_) => OpProof::ZkSig(ZkSignature::new(
                CompressedGroth16Proof::from_bytes(&[0u8; 128]),
            )),
            Op::SDPDeclare(_) => {
                let proof = ZkAndEd25519Proof {
                    zk_sig: ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
                    ed25519_sig: Ed25519Signature::zero(),
                };
                OpProof::ZkAndEd25519Sigs(proof)
            }
            other => unreachable!("unexpected genesis op in tests: {}", other.as_str()),
        }
    }

    // Helper function to create a basic signed transaction
    // Genesis transactions don't need verified proofs for Blob/Inscription ops
    fn create_trusted_tx(
        mut ops: Vec<Op>,
        op_proofs_vec: Vec<OpProof>,
    ) -> SignedOps<Preverified, GenesisMode> {
        let transfer_op = TransferOp::new(Inputs::empty(), Outputs::new([create_test_note(1000)]));
        let mut new_ops = vec![Op::Transfer(transfer_op)];
        new_ops.append(&mut ops);
        let mantle_tx = Ops::new_unchecked(new_ops);
        let mut op_proofs = OpProofs::from([OpProof::ZkSig(
            ZkKey::multi_sign(&[], &mantle_tx.hash().to_fr()).unwrap(),
        )]);
        for proof in op_proofs_vec {
            op_proofs.try_push(proof).unwrap();
        }
        SignedOps::from_parts_trusted(mantle_tx, op_proofs).unwrap()
    }

    #[test]
    fn test_inscription_fields() {
        // check inscription with channel id [1; 32] fails
        let tx = create_trusted_tx(
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
        let tx = create_trusted_tx(
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
        let tx = create_trusted_tx(
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
        let tx = create_trusted_tx(
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
                Some(Error::UnsupportedGenesisOp(vec![(2, "ChannelInscribe")])),
            ),
        ];

        // Execute all test cases
        for (ops, expected_err) in test_cases {
            let ops_proofs = ops.iter().map(placeholder_proof).collect::<Vec<_>>();
            let tx = create_trusted_tx(ops, ops_proofs);
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
            let ops_proofs = ops.iter().map(placeholder_proof).collect::<Vec<_>>();
            let tx = create_trusted_tx(ops, ops_proofs);
            let result = GenesisTx::from_tx(tx);
            match expected_err {
                Some(expected) => assert_eq!(result, Err(expected)),
                None => assert!(result.is_ok()),
            }
        }
    }

    #[test]
    fn test_genesis_tx_serde() {
        // Create a genesis transaction with inscription (no signature proof required)
        let signed_mantle_tx = create_trusted_tx(
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
    fn test_genesis_time_boundaries() {
        // The u32::MAX unix timestamp is the largest value `GenesisTime` accepts.
        let max = OffsetDateTime::from_unix_timestamp(i64::from(u32::MAX)).unwrap();
        assert_eq!(
            GenesisTime::try_from(max).unwrap(),
            GenesisTime::new(u32::MAX)
        );

        // One second past u32::MAX must be rejected.
        let overflow = OffsetDateTime::from_unix_timestamp(i64::from(u32::MAX) + 1).unwrap();
        assert!(GenesisTime::try_from(overflow).is_err());

        // Pre-epoch (negative) must be rejected.
        let pre_epoch = OffsetDateTime::from_unix_timestamp(-1).unwrap();
        assert!(GenesisTime::try_from(pre_epoch).is_err());
    }

    #[test]
    fn test_cryptarchia_parameter_decode_errors() {
        // A single byte is a complete 1-byte chain_id length prefix of 0, which
        // is below the chain_id minimum length of 1.
        assert!(matches!(
            CryptarchiaParameter::decode(&[0; 1], &()).unwrap_err(),
            DecodeError::LengthOutOfBounds { .. }
        ));

        // Genuinely too short: not even the length prefix can be read.
        assert!(matches!(
            CryptarchiaParameter::decode(&[], &()).unwrap_err(),
            DecodeError::UnexpectedEnd { .. }
        ));

        // Wrong length (chain_id_len says 100 but only a few bytes follow)
        let mut bad = vec![0; 48];
        bad[0] = 100; // chain_id_len = 100
        assert!(matches!(
            CryptarchiaParameter::decode(&bad, &()).unwrap_err(),
            DecodeError::UnexpectedEnd { .. }
        ));

        // Invalid UTF-8 chain_id. The chain_id bytes begin right after its
        // single-byte length prefix, so index 1 is the first UTF-8 byte.
        let mut encoded = cryptarchia_param().encode_to_vec();
        encoded[1] = 0xFF; // corrupt the first chain_id UTF-8 byte
        assert!(matches!(
            CryptarchiaParameter::decode(&encoded, &()).unwrap_err(),
            DecodeError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_genesis_tx_cryptarchia_parameter() {
        let param = cryptarchia_param();
        let tx = create_trusted_tx(
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
