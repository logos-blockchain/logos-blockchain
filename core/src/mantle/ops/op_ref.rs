use lb_binary_codec::canonical::BinaryEncode;
use serde::{Serialize, Serializer};

use crate::mantle::{
    Op,
    gas::{Gas, GasOverflow, GasProfile, OpGasCalculator as _, OpGasContext},
    ledger::ProvableOperation,
    ops::{
        channel::{
            config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp,
            withdraw::ChannelWithdrawOp,
        },
        inscribe::InscribeOp,
        internal::OpSer,
        pow::ClaimPowRewardOp,
        program::ProgramRegisterOp,
        sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
        transfer::TransferOp,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpRef<'a> {
    ChannelInscribe(&'a InscriptionOp),
    ChannelConfig(&'a ChannelConfigOp),
    ChannelDeposit(&'a DepositOp),
    ChannelWithdraw(&'a ChannelWithdrawOp),
    SDPDeclare(&'a SDPDeclareOp),
    SDPWithdraw(&'a SDPWithdrawOp),
    SDPActive(&'a SDPActiveOp),
    Transfer(&'a TransferOp),
    ClaimPowReward(&'a ClaimPowRewardOp),
    ProgramRegister(&'a ProgramRegisterOp),
    Inscribe(&'a InscribeOp),
}

const fn code_of<T>(_op: &T) -> u8
where
    T: ProvableOperation,
{
    T::CODE
}

impl OpRef<'_> {
    /// The Execution Gas of the Operation under the era's `profile`, against
    /// the thresholds in force in `context`.
    pub fn execution_gas(
        &self,
        profile: &GasProfile,
        context: &impl OpGasContext,
    ) -> Result<Gas, GasOverflow> {
        match self {
            Self::ChannelInscribe(op) => op.execution_gas(profile, context),
            Self::ChannelConfig(op) => op.execution_gas(profile, context),
            Self::ChannelDeposit(op) => op.execution_gas(profile, context),
            Self::ChannelWithdraw(op) => op.execution_gas(profile, context),
            Self::SDPDeclare(op) => op.execution_gas(profile, context),
            Self::SDPWithdraw(op) => op.execution_gas(profile, context),
            Self::SDPActive(op) => op.execution_gas(profile, context),
            Self::Transfer(op) => op.execution_gas(profile, context),
            Self::ClaimPowReward(op) => op.execution_gas(profile, context),
            Self::ProgramRegister(op) => op.execution_gas(profile, context),
            Self::Inscribe(op) => op.execution_gas(profile, context),
        }
    }

    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ChannelInscribe(_) => "ChannelInscribe",
            Self::ChannelConfig(_) => "ChannelConfig",
            Self::ChannelDeposit(_) => "ChannelDeposit",
            Self::ChannelWithdraw(_) => "ChannelWithdraw",
            Self::SDPDeclare(_) => "SDPDeclare",
            Self::SDPWithdraw(_) => "SDPWithdraw",
            Self::SDPActive(_) => "SDPActive",
            Self::Transfer(_) => "Transfer",
            Self::ClaimPowReward(_) => "ClaimPowReward",
            Self::ProgramRegister(_) => "ProgramRegister",
            Self::Inscribe(_) => "Inscribe",
        }
    }

    const fn code(&self) -> u8 {
        match self {
            Self::ChannelInscribe(op) => code_of(*op),
            Self::ChannelConfig(op) => code_of(*op),
            Self::ChannelDeposit(op) => code_of(*op),
            Self::ChannelWithdraw(op) => code_of(*op),
            Self::SDPDeclare(op) => code_of(*op),
            Self::SDPWithdraw(op) => code_of(*op),
            Self::SDPActive(op) => code_of(*op),
            Self::Transfer(op) => code_of(*op),
            Self::ClaimPowReward(op) => code_of(*op),
            Self::ProgramRegister(op) => code_of(*op),
            Self::Inscribe(op) => code_of(*op),
        }
    }
}

impl BinaryEncode for OpRef<'_> {
    fn encoded_length(&self) -> usize {
        let payload = match self {
            Self::ChannelInscribe(op) => op.encoded_length(),
            Self::ChannelConfig(op) => op.encoded_length(),
            Self::ChannelDeposit(op) => op.encoded_length(),
            Self::ChannelWithdraw(op) => op.encoded_length(),
            Self::SDPDeclare(op) => op.encoded_length(),
            Self::SDPWithdraw(op) => op.encoded_length(),
            Self::SDPActive(op) => op.encoded_length(),
            Self::Transfer(op) => op.encoded_length(),
            Self::ClaimPowReward(op) => op.encoded_length(),
            Self::ProgramRegister(op) => op.encoded_length(),
            Self::Inscribe(op) => op.encoded_length(),
        };
        self.code().encoded_length().checked_add(payload).unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.code().encode_into(out);
        match self {
            Self::ChannelInscribe(op) => op.encode_into(out),
            Self::ChannelConfig(op) => op.encode_into(out),
            Self::ChannelDeposit(op) => op.encode_into(out),
            Self::ChannelWithdraw(op) => op.encode_into(out),
            Self::SDPDeclare(op) => op.encode_into(out),
            Self::SDPWithdraw(op) => op.encode_into(out),
            Self::SDPActive(op) => op.encode_into(out),
            Self::Transfer(op) => op.encode_into(out),
            Self::ClaimPowReward(op) => op.encode_into(out),
            Self::ProgramRegister(op) => op.encode_into(out),
            Self::Inscribe(op) => op.encode_into(out),
        }
    }
}

macro_rules! impl_owned_ref_conversions {
    ($enum:ident => $($ty:ty => $variant:ident),* $(,)?) => {
        impl<'a> From<&'a $enum> for OpRef<'a> {
            fn from(op: &'a $enum) -> Self {
                match op { $($enum::$variant(op) => Self::$variant(op)),* }
            }
        }
        $(
            impl<'a> From<&'a $ty> for OpRef<'a> {
                fn from(op: &'a $ty) -> Self { Self::$variant(op) }
            }
        )*
    };
}

impl_owned_ref_conversions! {
    Op =>
        InscriptionOp => ChannelInscribe,
        ChannelConfigOp => ChannelConfig,
        DepositOp => ChannelDeposit,
        ChannelWithdrawOp => ChannelWithdraw,
        SDPDeclareOp => SDPDeclare,
        SDPWithdrawOp => SDPWithdraw,
        SDPActiveOp => SDPActive,
        TransferOp => Transfer,
        ClaimPowRewardOp => ClaimPowReward,
        ProgramRegisterOp => ProgramRegister,
        InscribeOp => Inscribe,
}

impl Serialize for OpRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let op_ser = OpSer::from(*self);
            op_ser.serialize(serializer)
        } else {
            let bytes = self.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}
