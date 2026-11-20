//! The `INSCRIBE` Operation: data the ledger carries and does not read, in no
//! channel.
//!
//! Spec: [Mantle: INSCRIBE](https://github.com/logos-co/logos-lips/pull/540)

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mantle::{
    gas::{Gas, GasOverflow, GasProfile, OpGasCalculator, OpGasContext},
    ledger::{
        PreverifiableOperation, ProvableOperation, VerifiableOperation,
        verification_mode::StandardMode,
    },
    ops::{
        NoOpProof, OpId, SignedOperation,
        channel::inscribe::{Inscription, serde_inscription},
    },
    transactions::states::{Preverified, Unverified},
};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryEncode)]
pub struct InscribeOp {
    /// From 1 byte to as many as the era allows an Operation, as the
    /// inscription of a channel.
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
}

impl BinaryDecode for InscribeOp {
    /// The most bytes the era the inscribe is decoded under allows its
    /// inscription.
    type Context = u32;

    fn decode<'input>(
        input: &'input [u8],
        max_op_data_size: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, inscription) = Inscription::decode(input, &(*max_op_data_size, ()))?;
        if inscription.is_empty() {
            return Err(DecodeError::invalid_value::<Self>(
                "the inscription of an inscribe holds at least one byte",
            ));
        }
        Ok((rest, Self { inscription }))
    }
}

impl InscribeOp {
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            inscription: Inscription::new_unchecked(b"inscribe".to_vec()),
        }
    }
}

impl OpId for InscribeOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

impl ProvableOperation for InscribeOp {
    type Proof = NoOpProof;
    const CODE: u8 = 0x02;
}

impl OpGasCalculator for InscribeOp {
    // The specification leaves `EXECUTION_INSCRIBE_GAS` to be determined: the
    // bytes of the inscription pay the storage fee, and the Operation nothing.
    fn execution_gas(
        &self,
        _profile: &GasProfile,
        _context: &impl OpGasContext,
    ) -> Result<Gas, GasOverflow> {
        Ok(Gas::new(0))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InscribeError {
    #[error("The inscription of an inscribe is empty")]
    EmptyInscription,
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<InscribeOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = InscribeError;

    // An inscribe built without its decoder, from JSON, can be empty.
    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        if self.operation().inscription.is_empty() {
            return Err(InscribeError::EmptyInscription);
        }
        Ok(())
    }
}

impl VerifiableOperation<StandardMode> for SignedOperation<InscribeOp, Preverified, StandardMode> {
    type DeferredProof = ();
    type Context<'a> = ();
    type Error = InscribeError;

    fn verify(&self, _context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mantle::{Op, OpProof};

    // Mantle test vectors, Operation Id and Delivery Hash: the delivery
    // inscription of the first note encryption vector.
    const DELIVERY_INSCRIPTION: &str = "4e4f54455f44454c49564552595f5631a352841f541aba1aab7d3abbb1eb4487e71756050f5c5acb757a62f8d8a14f91";
    const OP_ID: &str = "e504b22bf0d443e396b15d20b3a4a5d9f650ae82c54169fe6b58572f541fa5ca";

    fn inscribe(inscription: Vec<u8>) -> InscribeOp {
        InscribeOp {
            inscription: Inscription::new_unchecked(inscription),
        }
    }

    #[test]
    fn payload_and_op_id_match_the_spec_vector() {
        let op = inscribe(hex::decode(DELIVERY_INSCRIPTION).unwrap());

        assert_eq!(
            hex::encode(op.encode()),
            format!("30000000{DELIVERY_INSCRIPTION}")
        );
        assert_eq!(hex::encode(op.op_id()), OP_ID);

        let encoded = Op::Inscribe(op.clone()).encode();
        assert_eq!(encoded[0], 0x02);
        assert_eq!(
            Op::decode(&encoded, &u32::MAX).unwrap(),
            (&[][..], Op::Inscribe(op.clone()))
        );
        assert_eq!(Op::Inscribe(op).sample_proof(), OpProof::None(NoOpProof));
    }

    #[test]
    fn decode_refuses_an_empty_inscription_and_one_above_the_bound() {
        assert!(InscribeOp::decode(&inscribe(Vec::new()).encode(), &4).is_err());
        assert!(InscribeOp::decode(&inscribe(vec![1; 5]).encode(), &4).is_err());
        assert_eq!(
            InscribeOp::decode(&inscribe(vec![1; 4]).encode(), &4).unwrap(),
            (&[][..], inscribe(vec![1; 4]))
        );
    }

    #[test]
    fn preverify_refuses_an_empty_inscription() {
        let signed = |inscription| {
            SignedOperation::<_, Unverified, StandardMode>::new(inscribe(inscription), NoOpProof)
        };

        assert_eq!(
            signed(Vec::new()).into_preverified(&()).err(),
            Some(InscribeError::EmptyInscription)
        );
        assert!(signed(vec![1]).into_preverified(&()).is_ok());
    }

    #[test]
    fn json_keeps_the_opcode_and_the_inscription() {
        let op = Op::Inscribe(inscribe(b"note".to_vec()));
        let json = serde_json::to_value(&op).unwrap();

        assert_eq!(json.get("opcode"), Some(&serde_json::json!(0x02)));
        assert_eq!(serde_json::from_value::<Op>(json).unwrap(), op);
    }
}
