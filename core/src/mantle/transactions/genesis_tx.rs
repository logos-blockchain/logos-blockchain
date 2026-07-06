use core::fmt::{self, Display, Formatter};

use lb_core_macros::NomCodec;
use lb_groth16::Fr;
use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult,
    error::{Error as NomError, ErrorKind},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{SignedMantleTx, TxHash};
use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{
        MantleTx, OpProof, Transaction, TransactionHasher,
        gas::{Gas, GasCalculator, GasConstants, GasCost, GasOverflow, GasPrice},
        nom::{NomDecode, NomEncode},
        ops::{
            Op,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
            codec::proof_matches,
            sdp::SDPDeclareOp,
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
    #[error("Too many operations in genesis transaction: {count}")]
    TooManyOps { count: usize },
    #[error(
        "Genesis transaction has a different number of operations ({ops_count}) and proofs ({proofs_count})"
    )]
    ProofCountMismatch {
        ops_count: usize,
        proofs_count: usize,
    },
    #[error(
        "Genesis transaction operation ({op:?}) is paired with an invalid proof type ({proof:?})"
    )]
    MismatchedProofType { op: Box<Op>, proof: Box<OpProof> },
}

impl GenesisTx {
    pub fn from_tx(signed_mantle_tx: SignedMantleTx) -> Result<Self, Error> {
        let mantle_tx = &signed_mantle_tx.mantle_tx;

        // Genesis transactions must contain exactly one transfer as the first op,
        // one inscription as the second op, and then may contain other SDP declarations
        let cryptarchia_parameter = match mantle_tx.ops().as_slice() {
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

        // Validate that every operation is paired with a proof of the correct variant.
        let ops_count = signed_mantle_tx.mantle_tx.ops().len();
        let proofs_count = signed_mantle_tx.ops_proofs.len();
        if ops_count != proofs_count {
            return Err(Error::ProofCountMismatch {
                ops_count,
                proofs_count,
            });
        }
        for (proof, op) in signed_mantle_tx
            .ops_proofs
            .iter()
            .zip(signed_mantle_tx.mantle_tx.ops().iter())
        {
            if !proof_matches(proof, op) {
                return Err(Error::MismatchedProofType {
                    op: Box::new(op.clone()),
                    proof: Box::new(proof.clone()),
                });
            }
        }

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

    Ok(
        CryptarchiaParameter::decode(inscription.inscription.as_ref())
            .map_err(|e| Error::InvalidCryptarchiaParameter(format!("Decoding error: {e}")))?
            .1,
    )
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
    fn genesis_transfer(&self) -> &TransferOp {
        // Safe to unwrap because we validated this in from_tx
        match &self.mantle_tx().ops()[0] {
            Op::Transfer(op) => op,
            _ => unreachable!("GenesisTx always has a valid transfer as first op"),
        }
    }

    fn genesis_inscription(&self) -> &InscriptionOp {
        // Safe to unwrap because we validated this in from_tx
        match &self.mantle_tx().ops()[1] {
            Op::ChannelInscribe(op) => op,
            _ => unreachable!("GenesisTx always has a valid inscription as second op"),
        }
    }

    fn cryptarchia_parameter(&self) -> CryptarchiaParameter {
        self.cryptarchia_parameter.clone()
    }

    fn sdp_declarations(&self) -> impl Iterator<Item = (&SDPDeclareOp, &OpProof)> {
        self.mantle_tx()
            .ops()
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
struct BoundedString<const MIN_SIZE: usize, const MAX_SIZE: usize>(String);

impl<const MIN_SIZE: usize, const MAX_SIZE: usize> AsRef<str>
    for BoundedString<MIN_SIZE, MAX_SIZE>
{
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<const MIN_SIZE: usize, const MAX_SIZE: usize> Display for BoundedString<MIN_SIZE, MAX_SIZE> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<const MIN_SIZE: usize, const MAX_SIZE: usize> TryFrom<String>
    for BoundedString<MIN_SIZE, MAX_SIZE>
{
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() < MIN_SIZE {
            return Err(format!("String must be at least {MIN_SIZE} bytes: {value}"));
        }
        if value.len() > MAX_SIZE {
            return Err(format!("String must not exceed {MAX_SIZE} bytes: {value}"));
        }

        Ok(Self(value))
    }
}

impl<const MIN_SIZE: usize, const MAX_SIZE: usize> TryFrom<Vec<u8>>
    for BoundedString<MIN_SIZE, MAX_SIZE>
{
    type Error = String;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        let string = String::from_utf8(value).map_err(|e| format!("Invalid UTF-8 string: {e}"))?;
        Self::try_from(string)
    }
}

impl<const MIN_SIZE: usize, const MAX_SIZE: usize, const MIN: usize, const MAX: usize>
    TryFrom<BoundedVec<u8, MIN, MAX>> for BoundedString<MIN_SIZE, MAX_SIZE>
{
    type Error = String;

    fn try_from(value: BoundedVec<u8, MIN, MAX>) -> Result<Self, Self::Error> {
        const {
            assert!(
                MIN >= MIN_SIZE,
                "Min size cannot be less than the minimum allowed byte size for a chain ID."
            );
            assert!(
                MAX <= MAX_SIZE,
                "Max size cannot be more than the maximum allowed byte size for a chain ID."
            );
        }
        Self::try_from(value.into_inner())
    }
}

pub const MAX_CHAIN_ID_SIZE: usize = u64::MAX as usize;
type ChainIdBoundedVec = BoundedVec<u8, 1, MAX_CHAIN_ID_SIZE>;
type ChainIdBoundedString = BoundedString<1, MAX_CHAIN_ID_SIZE>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChainId(ChainIdBoundedString);

impl ChainId {
    #[must_use]
    pub const fn new_unchecked(addr: String) -> Self {
        Self(BoundedString(addr))
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0.0
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.0.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.0.is_empty()
    }
}

impl Display for ChainId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for ChainId {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

impl AsRef<[u8]> for ChainId {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref().as_ref()
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
            // No need to assert `MAX <= MAX_CHAIN_ID_SIZE` because
            // `MAX_CHAIN_ID_SIZE` == `u64::MAX` so comparison is
            // always true, and the compiles throws an error because
            // of that.
        }
        Self::try_from(value.into_inner())
    }
}

impl NomEncode for ChainId {
    fn encode(&self) -> Vec<u8> {
        let bounded_bytes =
            ChainIdBoundedVec::new_unchecked(<Self as AsRef<[u8]>>::as_ref(self).to_owned());
        bounded_bytes.encode()
    }
}

impl NomDecode for ChainId {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, value) = ChainIdBoundedVec::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(NomError::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}

/// Time at which the chain should start. u32 suffices: we only need the
/// positive half of the i64 Unix timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, NomCodec)]
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
#[derive(Debug, Clone, PartialEq, Eq, NomCodec)]
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
            ledger::{Inputs, Note, Outputs, Utxo, Value},
            ops::channel::{Ed25519PublicKey, inscribe::Inscription},
            transactions::Ops,
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
            inscription: Inscription::new_unchecked(cryptarchia_param.encode()),
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
            locked_note_id: utxo_to_use.id(),
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
            Op::SDPDeclare(_) => OpProof::ZkAndEd25519Sigs {
                zk_sig: ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
                ed25519_sig: Ed25519Signature::zero(),
            },
            other => unreachable!("unexpected genesis op in tests: {}", other.as_str()),
        }
    }

    // Helper function to create a basic signed transaction
    // Genesis transactions don't need verified proofs for Blob/Inscription ops
    fn create_tx(mut ops: Vec<Op>, mut ops_proofs: Vec<OpProof>) -> SignedMantleTx {
        let transfer_op = TransferOp::new(Inputs::empty(), Outputs::new([create_test_note(1000)]));
        let mut new_ops = vec![Op::Transfer(transfer_op)];
        new_ops.append(&mut ops);
        let mantle_tx = MantleTx(Ops::new_unchecked(new_ops));
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
            let ops_proofs = ops.iter().map(placeholder_proof).collect::<Vec<_>>();
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
            let ops_proofs = ops.iter().map(placeholder_proof).collect::<Vec<_>>();
            let tx = create_tx(ops, ops_proofs);
            let result = GenesisTx::from_tx(tx);
            match expected_err {
                Some(expected) => assert_eq!(result, Err(expected)),
                None => assert!(result.is_ok()),
            }
        }
    }

    #[test]
    fn test_genesis_op_proof_type_mismatch() {
        let inscription_op = inscription_op(
            ChannelId::from([0; 32]),
            &cryptarchia_param(),
            MsgId::root(),
            Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
        );
        let utxo = Utxo::new([0u8; 32], 0, create_test_note(1000));
        let verifying_key = Ed25519PublicKey::from_bytes(&[0; 32]).unwrap();
        let sdp_op = sdp_declare_op(utxo, 0, verifying_key);
        // SDPDeclare requires a `ZkAndEd25519Sigs` proof, an `Ed25519Sig` is the
        // wrong variant and must be rejected
        let tx = create_tx(
            vec![Op::ChannelInscribe(inscription_op), Op::SDPDeclare(sdp_op)],
            vec![
                OpProof::Ed25519Sig(Ed25519Signature::zero()),
                OpProof::Ed25519Sig(Ed25519Signature::zero()),
            ],
        );
        assert!(matches!(
            GenesisTx::from_tx(tx),
            Err(Error::MismatchedProofType { .. })
        ));
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
        let (_, decoded) = CryptarchiaParameter::decode(&encoded).unwrap();
        assert_eq!(param, decoded);
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
        // Too short
        assert!(matches!(
            CryptarchiaParameter::decode(&[0; 1]).unwrap_err(),
            nom::Err::Error(NomError {
                code: ErrorKind::Eof,
                ..
            })
        ));

        // Wrong length (chain_id_len says 100 but only a few bytes follow)
        let mut bad = vec![0; 48];
        bad[0] = 100; // chain_id_len = 100
        assert!(matches!(
            CryptarchiaParameter::decode(&bad).unwrap_err(),
            nom::Err::Error(NomError {
                code: ErrorKind::Eof,
                ..
            })
        ));

        // Invalid UTF-8 chain_id
        let mut encoded = cryptarchia_param().encode();
        encoded[8] = 0xFF; // corrupt the UTF-8 byte
        assert!(matches!(
            CryptarchiaParameter::decode(&encoded).unwrap_err(),
            nom::Err::Error(NomError {
                code: ErrorKind::MapRes,
                ..
            })
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
