use lb_codec::BinaryEncode;
use serde::{Serialize, Serializer};

use crate::mantle::{
    GasProfile, Op,
    gas::{Gas, OperationGas},
    ledger::ProvableOperation,
    ops::{
        channel::{
            channel_transfer::ChannelTransferOp, config::ChannelConfigOp, deposit::DepositOp,
            inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
        },
        internal::OpSer,
        leader_claim::LeaderClaimOp,
        pow::ClaimPowRewardOp,
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
    ChannelTransfer(&'a ChannelTransferOp),
    SDPDeclare(&'a SDPDeclareOp),
    SDPWithdraw(&'a SDPWithdrawOp),
    SDPActive(&'a SDPActiveOp),
    LeaderClaim(&'a LeaderClaimOp),
    Transfer(&'a TransferOp),
    ClaimPowReward(&'a ClaimPowRewardOp),
}

const fn gas_cost_of<T, Profile>(_op: &T) -> Gas
where
    T: OperationGas<Profile>,
    Profile: GasProfile,
{
    T::GAS_COST
}

const fn code_of<T>(_op: &T) -> u8
where
    T: ProvableOperation,
{
    T::CODE
}

impl OpRef<'_> {
    #[must_use]
    pub const fn gas_cost<Profile: GasProfile>(&self) -> Gas {
        match self {
            Self::ChannelInscribe(op) => gas_cost_of(op),
            Self::ChannelConfig(op) => gas_cost_of(op),
            Self::ChannelDeposit(op) => gas_cost_of(op),
            Self::ChannelWithdraw(op) => gas_cost_of(op),
            Self::ChannelTransfer(op) => gas_cost_of(op),
            Self::SDPDeclare(op) => gas_cost_of(op),
            Self::SDPWithdraw(op) => gas_cost_of(op),
            Self::SDPActive(op) => gas_cost_of(op),
            Self::LeaderClaim(op) => gas_cost_of(op),
            Self::Transfer(op) => gas_cost_of(op),
            Self::ClaimPowReward(op) => gas_cost_of(op),
        }
    }

    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ChannelInscribe(_) => "ChannelInscribe",
            Self::ChannelConfig(_) => "ChannelConfig",
            Self::ChannelDeposit(_) => "ChannelDeposit",
            Self::ChannelWithdraw(_) => "ChannelWithdraw",
            Self::ChannelTransfer(_) => "ChannelTransfer",
            Self::SDPDeclare(_) => "SDPDeclare",
            Self::SDPWithdraw(_) => "SDPWithdraw",
            Self::SDPActive(_) => "SDPActive",
            Self::LeaderClaim(_) => "LeaderClaim",
            Self::Transfer(_) => "Transfer",
            Self::ClaimPowReward(_) => "ClaimPowReward",
        }
    }

    const fn code(&self) -> u8 {
        match self {
            Self::ChannelInscribe(op) => code_of(*op),
            Self::ChannelConfig(op) => code_of(*op),
            Self::ChannelDeposit(op) => code_of(*op),
            Self::ChannelWithdraw(op) => code_of(*op),
            Self::ChannelTransfer(op) => code_of(*op),
            Self::SDPDeclare(op) => code_of(*op),
            Self::SDPWithdraw(op) => code_of(*op),
            Self::SDPActive(op) => code_of(*op),
            Self::LeaderClaim(op) => code_of(*op),
            Self::Transfer(op) => code_of(*op),
            Self::ClaimPowReward(op) => code_of(*op),
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
            Self::ChannelTransfer(op) => op.encoded_length(),
            Self::SDPDeclare(op) => op.encoded_length(),
            Self::SDPWithdraw(op) => op.encoded_length(),
            Self::SDPActive(op) => op.encoded_length(),
            Self::LeaderClaim(op) => op.encoded_length(),
            Self::Transfer(op) => op.encoded_length(),
            Self::ClaimPowReward(op) => op.encoded_length(),
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
            Self::ChannelTransfer(op) => op.encode_into(out),
            Self::SDPDeclare(op) => op.encode_into(out),
            Self::SDPWithdraw(op) => op.encode_into(out),
            Self::SDPActive(op) => op.encode_into(out),
            Self::LeaderClaim(op) => op.encode_into(out),
            Self::Transfer(op) => op.encode_into(out),
            Self::ClaimPowReward(op) => op.encode_into(out),
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
        ChannelTransferOp => ChannelTransfer,
        SDPDeclareOp => SDPDeclare,
        SDPWithdrawOp => SDPWithdraw,
        SDPActiveOp => SDPActive,
        LeaderClaimOp => LeaderClaim,
        TransferOp => Transfer,
        ClaimPowRewardOp => ClaimPowReward,
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
