use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
#[cfg(any(test, feature = "test-utils"))]
use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(any(test, feature = "test-utils"))]
use crate::mantle::OpProof;
#[cfg(any(test, feature = "test-utils"))]
use crate::mantle::ops::{
    ChannelInscribeProof, StepProof,
    op_proof::samples::{SampleProof as _, sample_proof_for},
};
use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        GasProfile,
        gas::Gas,
        ledger::ProvableOperation as _,
        ops::{
            OPERATION_ID_V1, OpRef,
            channel::{
                config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp,
                withdraw::ChannelWithdrawOp,
            },
            internal::{OpDe, OpSer},
            pow::ClaimPowRewardOp,
            program::ProgramRegisterOp,
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
    },
};

pub trait OpId {
    fn op_id(&self) -> Hash {
        let mut encoded_bytes = OPERATION_ID_V1.clone();
        encoded_bytes.extend(self.op_bytes());
        Hasher::digest(&encoded_bytes).into()
    }

    fn op_bytes(&self) -> Vec<u8>;
}

impl<T: OpId> OpId for &T {
    fn op_id(&self) -> Hash {
        (*self).op_id()
    }

    fn op_bytes(&self) -> Vec<u8> {
        (*self).op_bytes()
    }
}

/// Core set of supported Mantle operations.
///
/// This type serves as the public-facing representation of [`OpSer`] and
/// [`OpDe`], delegating default serialization and deserialization to them.
///
/// Serialization and deserialization share a single [`serde_::OpWire`] wire
/// shape, which carries an `opcode` tag used to identify the correct variant.
/// Due to limitations in [`bincode`] and [`serde`]'s `#[serde(untagged)]`
/// enums, binary deserialization is routed through [`decode_op`] instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Op {
    ChannelInscribe(InscriptionOp),
    ChannelConfig(ChannelConfigOp),
    ChannelDeposit(DepositOp),
    ChannelWithdraw(ChannelWithdrawOp),
    SDPDeclare(SDPDeclareOp),
    SDPWithdraw(SDPWithdrawOp),
    SDPActive(SDPActiveOp),
    Transfer(TransferOp),
    ClaimPowReward(ClaimPowRewardOp),
    ProgramRegister(ProgramRegisterOp),
}

/// Delegates serialization through the [`OpInternal`] representation.
impl Serialize for Op {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let op_ser = OpSer::from(self);
            op_ser.serialize(serializer)
        } else {
            let bytes = self.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}

/// Delegates deserialization through the [`OpDe`] representation.
///
/// If the deserializer is non-human-readable it falls back into custom
/// decoding via [`decode_op`]. Otherwise, it deserializes via [`OpDe`]'s
/// default behaviour.
impl<'de> Deserialize<'de> for Op {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            OpDe::deserialize(deserializer).map(Self::from)
        } else {
            let bytes = <Vec<u8>>::deserialize(deserializer)?;
            Self::decode(&bytes, &())
                .map(|(_, op)| op)
                .map_err(serde::de::Error::custom)
        }
    }
}

// Op = Opcode OpPayload
impl BinaryEncode for Op {
    fn encoded_length(&self) -> usize {
        self.by_ref().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.by_ref().encode_into(out);
    }
}

impl BinaryDecode for Op {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, opcode) = u8::decode(input, &())?;

        match opcode {
            InscriptionOp::CODE => InscriptionOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelInscribe(op))),
            ChannelConfigOp::CODE => ChannelConfigOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelConfig(op))),
            DepositOp::CODE => {
                DepositOp::decode(input, &()).map(|(rest, op)| (rest, Self::ChannelDeposit(op)))
            }
            ChannelWithdrawOp::CODE => ChannelWithdrawOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelWithdraw(op))),
            SDPDeclareOp::CODE => {
                SDPDeclareOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPDeclare(op)))
            }
            SDPWithdrawOp::CODE => {
                SDPWithdrawOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPWithdraw(op)))
            }
            SDPActiveOp::CODE => {
                SDPActiveOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPActive(op)))
            }
            TransferOp::CODE => {
                TransferOp::decode(input, &()).map(|(rest, op)| (rest, Self::Transfer(op)))
            }
            ClaimPowRewardOp::CODE => ClaimPowRewardOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ClaimPowReward(op))),
            ProgramRegisterOp::CODE => ProgramRegisterOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ProgramRegister(op))),
            other => Err(DecodeError::unknown_discriminant::<Self>(u64::from(other))),
        }
    }
}

impl Op {
    #[must_use]
    pub fn by_ref(&self) -> OpRef<'_> {
        self.into()
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.by_ref().as_str()
    }

    #[must_use]
    pub fn gas_cost<Profile: GasProfile>(&self) -> Gas {
        self.by_ref().gas_cost::<Profile>()
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample_proof(&self) -> OpProof {
        match self {
            // One step proof per step, so a `SignedOps` sample decodes.
            Self::ChannelInscribe(op) => OpProof::ChannelInscribe(ChannelInscribeProof::new(
                Ed25519Signature::sample(),
                op.steps
                    .iter()
                    .map(|step| StepProof::sample_for(step.proof_shape()))
                    .collect(),
            )),
            Self::ChannelConfig(op) => sample_proof_for(op),
            Self::ChannelDeposit(op) => sample_proof_for(op),
            Self::ChannelWithdraw(op) => sample_proof_for(op),
            Self::SDPDeclare(op) => sample_proof_for(op),
            Self::SDPWithdraw(op) => sample_proof_for(op),
            Self::SDPActive(op) => sample_proof_for(op),
            Self::Transfer(op) => sample_proof_for(op),
            Self::ClaimPowReward(op) => sample_proof_for(op),
            Self::ProgramRegister(op) => sample_proof_for(op),
        }
    }
}

macro_rules! impl_from_operation {
    ($($variant:ident => $operation:ident),* $(,)?) => {
        $(
            impl From<$operation> for Op {
                fn from(op: $operation) -> Self {
                    Self::$variant(op)
                }
            }
        )*
    };
}

impl_from_operation! {
    ChannelInscribe => InscriptionOp,
    ChannelConfig => ChannelConfigOp,
    ChannelDeposit => DepositOp,
    ChannelWithdraw => ChannelWithdrawOp,
    SDPDeclare => SDPDeclareOp,
    SDPWithdraw => SDPWithdrawOp,
    SDPActive => SDPActiveOp,
    Transfer => TransferOp,
    ClaimPowReward => ClaimPowRewardOp,
    ProgramRegister => ProgramRegisterOp,
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::canonical::{BinaryDecode as _, DecodeError};

    use crate::mantle::{
        Op, OpProof, fixtures::ops::op_values::TRANSFER, ledger::ProvableOperation as _,
        ops::transfer::TransferOp, transactions::Ops,
    };

    #[test]
    fn serialize_to_json_tags_the_payload_with_the_opcode() {
        let op = Op::Transfer(TRANSFER.clone());
        let encoded_op = serde_json::to_value(&op).expect("the human-readable arm serializes");

        assert_eq!(
            encoded_op.get("opcode").expect("the json has an opcode"),
            &serde_json::json!(TransferOp::CODE)
        );
    }

    #[test]
    fn serialize_to_binary_leads_the_payload_with_the_opcode() {
        let op = Op::Transfer(TRANSFER.clone());
        let envelope = bincode::serialize(&op).expect("the binary arm serializes");
        let encoded_op = &envelope[size_of::<u64>()..];

        assert_eq!(encoded_op.first(), Some(&TransferOp::CODE));
    }

    #[test]
    fn deserialize_from_json_selects_the_variant_matching_the_opcode() {
        let op = Op::Transfer(TRANSFER.clone());
        let encoded_op = serde_json::to_value(&op).expect("the human-readable arm serializes");
        let decoded_op =
            serde_json::from_value::<Op>(encoded_op).expect("the human-readable arm deserializes");

        assert!(matches!(decoded_op, Op::Transfer(_)));
    }

    #[test]
    fn deserialize_from_binary_reads_the_encoded_operation() {
        let op = Op::Transfer(TRANSFER.clone());
        let encoded_op = bincode::serialize(&op).expect("the binary arm serializes");
        let decoded_op =
            bincode::deserialize::<Op>(&encoded_op).expect("the binary arm deserializes");

        assert!(matches!(decoded_op, Op::Transfer(_)));
    }

    #[test]
    fn decode_rejects_the_retired_channel_transfer_opcode() {
        assert_eq!(
            Op::decode(&[0x14], &()).unwrap_err(),
            DecodeError::unknown_discriminant::<Op>(0x14)
        );
    }

    #[test]
    fn leader_claim_opcode_is_no_operation() {
        use lb_binary_codec::canonical::BinaryDecode as _;

        // 0x40 was LEADER_CLAIM, followed by its 96 byte payload.
        let mut bytes = vec![0u8; 97];
        bytes[0] = 0x40;
        assert!(Op::decode(&bytes, &()).is_err());
    }

    #[expect(clippy::match_same_arms, reason = "Clarity")]
    fn does_proof_match(op: &Op, op_proof: &OpProof) -> bool {
        match op {
            Op::ChannelInscribe(_) => matches!(op_proof, OpProof::ChannelInscribe(_)),
            Op::ChannelConfig(_) => matches!(op_proof, OpProof::ChannelMultiSigProof(_)),
            Op::ChannelDeposit(_) => matches!(op_proof, OpProof::ZkTransfer(_)),
            Op::ChannelWithdraw(_) => matches!(op_proof, OpProof::ZkTransfer(_)),
            Op::SDPDeclare(_) => matches!(op_proof, OpProof::ZkAndEd25519Sigs(_)),
            Op::SDPWithdraw(_) => matches!(op_proof, OpProof::ZkAndEd25519Sigs(_)),
            Op::SDPActive(_) => matches!(op_proof, OpProof::Ed25519Sig(_)),
            Op::Transfer(_) => matches!(op_proof, OpProof::ZkTransfer(_)),
            Op::ClaimPowReward(_) => matches!(op_proof, OpProof::None(_)),
            Op::ProgramRegister(_) => matches!(op_proof, OpProof::None(_)),
        }
    }

    #[test]
    fn sample_proof_matches_the_kind_each_op_requires() {
        for op in &Ops::sample() {
            let proof = op.sample_proof();

            assert!(
                does_proof_match(op, &proof),
                "{} got the wrong proof kind: {proof:?}",
                op.as_str()
            );
        }
    }

    #[test]
    fn transfer_deposit_and_withdraw_carry_a_zk_transfer_proof() {
        use lb_binary_codec::canonical::BinaryEncode as _;

        use crate::mantle::ops::{
            ZkTransferProof,
            channel::{deposit::DepositOp, withdraw::ChannelWithdrawOp},
            op_proof::samples::SampleProof as _,
        };

        for op in [
            Op::Transfer(TransferOp::sample()),
            Op::ChannelDeposit(DepositOp::sample()),
            Op::ChannelWithdraw(ChannelWithdrawOp::sample()),
        ] {
            let proof = op.sample_proof();
            assert_eq!(
                proof,
                OpProof::ZkTransfer(ZkTransferProof::sample()),
                "{}",
                op.as_str()
            );
            assert_eq!(proof.encode().len(), 128, "{}", op.as_str());
        }
    }

    #[test]
    fn sdp_declare_and_withdraw_carry_a_zk_transfer_then_a_signature() {
        use lb_binary_codec::canonical::BinaryEncode as _;
        use lb_groth16::CompressedGroth16Proof;
        use lb_key_management_system_keys::keys::Ed25519Signature;

        use crate::mantle::ops::{ZkAndEd25519Proof, ZkTransferProof};

        let proof = ZkAndEd25519Proof::new(
            ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[1u8; 128])),
            Ed25519Signature::from_bytes(&[2u8; 64]),
        );
        let mut expected = vec![1u8; 128];
        expected.extend([2u8; 64]);
        assert_eq!(proof.encode().to_vec(), expected);

        for op in [
            Op::SDPDeclare(crate::mantle::ops::sdp::SDPDeclareOp::sample()),
            Op::SDPWithdraw(crate::mantle::ops::sdp::SDPWithdrawOp::sample()),
        ] {
            assert!(
                matches!(op.sample_proof(), OpProof::ZkAndEd25519Sigs(_)),
                "{}",
                op.as_str()
            );
        }
    }

    #[test]
    fn execution_gas_follows_the_mantle_gas_table() {
        use crate::mantle::{
            gas::{Gas, MainnetGasProfile, test_utils::FixedThresholds},
            ops::{
                channel::{
                    config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp,
                    withdraw::ChannelWithdrawOp,
                },
                pow::ClaimPowRewardOp,
                program::ProgramRegisterOp,
                sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            },
        };

        // Every channel the thresholds are asked about requires 2 signatures.
        for (op, gas) in [
            // 2 inputs, 2 outputs: 30 + 1946 + 2 * 207 + 2 * 25
            (Op::Transfer(TransferOp::sample()), 2_440),
            (Op::ProgramRegister(ProgramRegisterOp::sample()), 6_843),
            // 18 + 2 * 167
            (Op::ChannelConfig(ChannelConfigOp::sample()), 352),
            // 13 + 167
            (Op::ChannelInscribe(InscriptionOp::sample()), 180),
            // 1 input, 1 output: 16 + 1946 + 207 + 25
            (Op::ChannelDeposit(DepositOp::sample()), 2_194),
            // 1 input, 1 output: 13 + 1946 + 207 + 25
            (Op::ChannelWithdraw(ChannelWithdrawOp::sample()), 2_191),
            // 669 + 167 + 1946 + 207 + 25
            (Op::SDPDeclare(SDPDeclareOp::sample()), 3_014),
            // 889 + 167 + 1946 + 207 + 25
            (Op::SDPWithdraw(SDPWithdrawOp::sample()), 3_234),
            // 6975 + 167
            (Op::SDPActive(SDPActiveOp::sample()), 7_142),
            (Op::ClaimPowReward(ClaimPowRewardOp::sample()), 569),
        ] {
            assert_eq!(
                op.by_ref()
                    .execution_gas::<MainnetGasProfile>(&FixedThresholds(2)),
                Ok(Gas::new(gas)),
                "{}",
                op.as_str()
            );
        }
    }

    #[test]
    fn opcodes_are_the_mantle_table() {
        use lb_binary_codec::canonical::{BinaryDecode as _, DecodeError};

        use crate::mantle::ops::{
            channel::{
                config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp,
                withdraw::ChannelWithdrawOp,
            },
            pow::ClaimPowRewardOp,
            program::ProgramRegisterOp,
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
        };

        let table = [
            TransferOp::CODE,
            ProgramRegisterOp::CODE,
            ChannelConfigOp::CODE,
            InscriptionOp::CODE,
            DepositOp::CODE,
            ChannelWithdrawOp::CODE,
            SDPDeclareOp::CODE,
            SDPWithdrawOp::CODE,
            SDPActiveOp::CODE,
            ClaimPowRewardOp::CODE,
        ];
        assert_eq!(
            table,
            [0x00, 0x01, 0x10, 0x11, 0x12, 0x13, 0x20, 0x21, 0x22, 0x30]
        );
        for opcode in 0..=u8::MAX {
            let unknown = Op::decode(&[opcode], &()).err()
                == Some(DecodeError::unknown_discriminant::<Op>(u64::from(opcode)));
            assert_eq!(unknown, !table.contains(&opcode), "opcode {opcode:#04x}");
        }
    }

    #[test]
    fn program_register_decodes_at_opcode_0x01() {
        use lb_binary_codec::canonical::{BinaryDecode as _, BinaryEncode as _};

        let op = Op::ProgramRegister(crate::mantle::ops::program::ProgramRegisterOp::sample());
        let bytes = op.encode();
        assert_eq!((bytes[0], bytes.len()), (0x01, 1 + 352));
        assert_eq!(Op::decode(&bytes, &()).unwrap(), (&[][..], op.clone()));
        assert_eq!(
            op.sample_proof(),
            OpProof::None(crate::mantle::ops::NoOpProof)
        );
    }

    #[test]
    fn field_element_not_less_than_the_modulus_is_rejected() {
        use lb_binary_codec::canonical::BinaryDecode as _;

        // TRANSFER with one input, no output, a zero root and a zero excess.
        let transfer = |nullifier: &str| {
            hex::decode(format!(
                "0001{nullifier}00{}{}",
                "00".repeat(32),
                "00".repeat(8)
            ))
            .unwrap()
        };
        let modulus = "010000f093f5e1439170b97948e833285d588181b64550b829a031e1724e6430";
        let below = "000000f093f5e1439170b97948e833285d588181b64550b829a031e1724e6430";

        assert!(Op::decode(&transfer(modulus), &()).is_err());
        let below = transfer(below);
        let (rest, op) = Op::decode(&below, &()).unwrap();
        assert_eq!(rest, [0u8; 0]);
        assert!(matches!(op, Op::Transfer(_)));
    }
}
