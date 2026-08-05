use crate::mantle::{
    ledger::verification_mode::VerificationMode,
    ops::{
        SignedOperation,
        channel::{
            channel_transfer::ChannelTransferOp, config::ChannelConfigOp, deposit::DepositOp,
            inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
        },
        leader_claim::LeaderClaimOp,
        sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
        transfer::TransferOp,
    },
    transactions::states::VerificationState,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignedOp<State: VerificationState, Mode: VerificationMode> {
    ChannelInscribe(SignedOperation<InscriptionOp, State, Mode>),
    ChannelConfig(SignedOperation<ChannelConfigOp, State, Mode>),
    ChannelDeposit(SignedOperation<DepositOp, State, Mode>),
    ChannelWithdraw(SignedOperation<ChannelWithdrawOp, State, Mode>),
    ChannelTransfer(SignedOperation<ChannelTransferOp, State, Mode>),
    SDPDeclare(SignedOperation<SDPDeclareOp, State, Mode>),
    SDPWithdraw(SignedOperation<SDPWithdrawOp, State, Mode>),
    SDPActive(SignedOperation<SDPActiveOp, State, Mode>),
    LeaderClaim(SignedOperation<LeaderClaimOp, State, Mode>),
    Transfer(SignedOperation<TransferOp, State, Mode>),
}
