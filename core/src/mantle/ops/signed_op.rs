use crate::mantle::{
    GasProfile, VerificationError,
    gas::{Gas, GasOverflow, OperationGas, SignedOperationExecutionGas as _},
    ledger::verification_mode::{StandardMode, VerificationMode},
    ops::{
        OpProofRef, OpRef, SignedOperation,
        channel::{
            channel_transfer::{ChannelTransferOp, ChannelTransferValidationContext},
            config::{ChannelConfigOp, ChannelConfigValidationContext},
            deposit::{DepositOp, DepositValidationContext},
            inscribe::{
                InscriptionOp, InscriptionPreverificationContext, InscriptionValidationContext,
            },
            withdraw::{ChannelWithdrawOp, WithdrawValidationContext},
        },
        leader_claim::{
            LeaderClaimOp, LeaderClaimPreverificationContext, LeaderClaimVerificationContext,
        },
        pow::{ClaimPoWRewardVerificationContext, ClaimPowRewardOp},
        sdp::{
            SDPActiveOp, SDPActiveValidationContext, SDPDeclareOp, SDPDeclareVerificationContext,
            SDPWithdrawOp, SDPWithdrawValidationContext, declare::SDPDeclarePreverificationContext,
        },
        transfer::{TransferOp, TransferValidationContext},
    },
    transactions::{
        OperationVerificationHelper,
        hash::TxHashView,
        states::{Preverified, Unverified, VerificationState, Verified},
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
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
    ClaimPowReward(SignedOperation<ClaimPowRewardOp, State, Mode>),
}

impl<State: VerificationState, Mode: VerificationMode> SignedOp<State, Mode> {
    #[must_use]
    pub fn operation(&self) -> OpRef<'_> {
        match self {
            Self::ChannelInscribe(signed_operation) => signed_operation.operation().into(),
            Self::ChannelConfig(signed_operation) => signed_operation.operation().into(),
            Self::ChannelDeposit(signed_operation) => signed_operation.operation().into(),
            Self::ChannelWithdraw(signed_operation) => signed_operation.operation().into(),
            Self::ChannelTransfer(signed_operation) => signed_operation.operation().into(),
            Self::SDPDeclare(signed_operation) => signed_operation.operation().into(),
            Self::SDPWithdraw(signed_operation) => signed_operation.operation().into(),
            Self::SDPActive(signed_operation) => signed_operation.operation().into(),
            Self::LeaderClaim(signed_operation) => signed_operation.operation().into(),
            Self::Transfer(signed_operation) => signed_operation.operation().into(),
            Self::ClaimPowReward(signed_operation) => signed_operation.operation().into(),
        }
    }

    #[must_use]
    pub fn proof(&self) -> OpProofRef<'_> {
        match self {
            Self::ChannelInscribe(signed_operation) => signed_operation.proof().into(),
            Self::ChannelConfig(signed_operation) => signed_operation.proof().into(),
            Self::ChannelDeposit(signed_operation) => signed_operation.proof().into(),
            Self::ChannelWithdraw(signed_operation) => signed_operation.proof().into(),
            Self::ChannelTransfer(signed_operation) => signed_operation.proof().into(),
            Self::SDPDeclare(signed_operation) => signed_operation.proof().into(),
            Self::SDPWithdraw(signed_operation) => signed_operation.proof().into(),
            Self::SDPActive(signed_operation) => signed_operation.proof().into(),
            Self::LeaderClaim(signed_operation) => signed_operation.proof().into(),
            Self::Transfer(signed_operation) => signed_operation.proof().into(),
            Self::ClaimPowReward(signed_operation) => signed_operation.proof().into(),
        }
    }

    pub fn execution_gas<Profile: GasProfile>(&self) -> Result<Gas, GasOverflow>
    where
        InscriptionOp: OperationGas<Profile>,
        ChannelConfigOp: OperationGas<Profile>,
        DepositOp: OperationGas<Profile>,
        ChannelWithdrawOp: OperationGas<Profile>,
        ChannelTransferOp: OperationGas<Profile>,
        SDPDeclareOp: OperationGas<Profile>,
        SDPWithdrawOp: OperationGas<Profile>,
        SDPActiveOp: OperationGas<Profile>,
        LeaderClaimOp: OperationGas<Profile>,
        TransferOp: OperationGas<Profile>,
        ClaimPowRewardOp: OperationGas<Profile>,
    {
        match self {
            Self::ChannelInscribe(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::ChannelConfig(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::ChannelDeposit(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::ChannelWithdraw(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::ChannelTransfer(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::SDPDeclare(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::SDPWithdraw(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::SDPActive(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::LeaderClaim(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::Transfer(signed_operation) => signed_operation.execution_gas::<Profile>(),
            Self::ClaimPowReward(signed_operation) => signed_operation.execution_gas::<Profile>(),
        }
    }

    /// Converts any variant of `SignedOp<State, Mode>` into a
    /// `SignedOp<NewState, Mode>` without performing any verification.
    ///
    /// This function is intended for
    /// [`GenesisTx`](crate::mantle::transactions::genesis_tx::GenesisTx) and
    /// testing purposes only.
    #[must_use]
    #[doc(hidden)]
    pub(crate) fn into_state_trusted<NewState: VerificationState>(
        self,
    ) -> SignedOp<NewState, Mode> {
        match self {
            Self::ChannelInscribe(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::ChannelConfig(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::ChannelDeposit(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::ChannelWithdraw(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::ChannelTransfer(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::SDPDeclare(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::SDPWithdraw(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::SDPActive(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::LeaderClaim(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::Transfer(signed_operation) => signed_operation.into_state_trusted().into(),
            Self::ClaimPowReward(signed_operation) => signed_operation.into_state_trusted().into(),
        }
    }
}

impl SignedOp<Unverified, StandardMode> {
    // TODO: Might drop proofs after verification.
    //  This is carried over from the original code.
    pub(crate) fn into_preverified(
        self,
        tx_hash_view: &TxHashView,
    ) -> Result<SignedOp<Preverified, StandardMode>, VerificationError> {
        match self {
            Self::ChannelInscribe(op) => {
                let context = InscriptionPreverificationContext { tx_hash_view };
                op.into_preverified(&context)
                    .map(SignedOp::from)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            Self::ChannelConfig(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::ChannelVerificationError),
            Self::ChannelDeposit(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::ChannelVerificationError),
            Self::ChannelWithdraw(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::ChannelVerificationError),
            Self::ChannelTransfer(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::ChannelVerificationError),
            Self::SDPDeclare(op) => {
                let context = SDPDeclarePreverificationContext { tx_hash_view };
                op.into_preverified(&context)
                    .map(SignedOp::from)
                    .map_err(VerificationError::SDPVerificationError)
            }
            Self::SDPWithdraw(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::SDPVerificationError),
            Self::SDPActive(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::SDPVerificationError),
            Self::LeaderClaim(op) => {
                let context = LeaderClaimPreverificationContext { tx_hash_view };
                op.into_preverified(&context)
                    .map(SignedOp::from)
                    .map_err(VerificationError::LeaderClaimVerificationError)
            }
            Self::Transfer(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::TransferVerificationError),
            Self::ClaimPowReward(op) => op
                .into_preverified(&())
                .map(SignedOp::from)
                .map_err(VerificationError::ClaimPowRewardError),
        }
    }
}

type VerifyFailure = (SignedOp<Preverified, StandardMode>, VerificationError);
fn map_verify_failure<SignedOperation, Error>(
    (signed_operation, error): (SignedOperation, Error),
) -> VerifyFailure
where
    SignedOperation: Into<SignedOp<Preverified, StandardMode>>,
    Error: Into<VerificationError>,
{
    (signed_operation.into(), error.into())
}

impl SignedOp<Preverified, StandardMode> {
    #[expect(
        clippy::result_large_err,
        reason = "Err returns the original operation and the error. Might address later."
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "The match arms are long due to the validation context construction. Split later."
    )]
    pub(crate) fn into_verified(
        self,
        op_index: usize,
        tx_hash_view: &TxHashView,
        helper: &impl OperationVerificationHelper,
    ) -> Result<SignedOp<Verified, StandardMode>, (Self, VerificationError)> {
        match self {
            Self::ChannelInscribe(op) => {
                let channel_inscribe_context = InscriptionValidationContext {
                    channels: helper.get_channels(),
                    block_slot: helper.get_block_slot(),
                };
                op.into_verified(&channel_inscribe_context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::ChannelConfig(op) => {
                let channel_config_context = ChannelConfigValidationContext {
                    channels: helper.get_channels(),
                    tx_hash_view,
                };
                op.into_verified(&channel_config_context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::ChannelDeposit(op) => {
                let channel_deposit_context = DepositValidationContext {
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                };
                op.into_verified(&channel_deposit_context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::ChannelWithdraw(op) => {
                let channel_withdraw_context = WithdrawValidationContext {
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                    helper,
                    op_index,
                };
                op.into_verified(&channel_withdraw_context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::ChannelTransfer(op) => {
                let context = ChannelTransferValidationContext {
                    locked_notes: helper.get_locked_notes(),
                    channels: helper.get_channels(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                    op_index,
                    helper,
                };
                op.into_verified(&context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::SDPDeclare(op) => {
                let declarations =
                    match helper.get_declarations_by_service(op.operation().service_type) {
                        Ok(declarations) => declarations,
                        Err(error) => return Err(map_verify_failure((op, error))),
                    };
                let context = SDPDeclareVerificationContext {
                    utxo_tree: helper.get_utxos(),
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    tx_hash_view,
                    declarations,
                    min_stake: helper.get_min_stake(),
                };
                op.into_verified(&context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::SDPWithdraw(op) => {
                let declarations =
                    match helper.get_declarations_by_id(&op.operation().declaration_id) {
                        Ok(declarations) => declarations,
                        Err(error) => return Err(map_verify_failure((op, error))),
                    };
                let context = SDPWithdrawValidationContext {
                    declarations,
                    epoch: helper.get_epoch(),
                    locked_notes: helper.get_locked_notes(),
                    tx_hash_view,
                };
                op.into_verified(&context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::SDPActive(op) => {
                let declarations =
                    match helper.get_declarations_by_id(&op.operation().declaration_id) {
                        Ok(declarations) => declarations,
                        Err(error) => return Err(map_verify_failure((op, error))),
                    };
                let context = SDPActiveValidationContext {
                    declarations,
                    tx_hash_view,
                    epoch: helper.get_epoch(),
                };
                op.into_verified(&context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::LeaderClaim(op) => {
                let context = LeaderClaimVerificationContext {
                    nullifiers: helper.get_nullifiers(),
                    claimable_vouchers_root: helper.get_claimable_vouchers_root(),
                    tx_hash_view,
                };
                op.into_verified(&context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::Transfer(op) => {
                let context = TransferValidationContext {
                    locked_notes: helper.get_locked_notes(),
                    channels: helper.get_channels(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                };
                op.into_verified(&context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
            Self::ClaimPowReward(op) => {
                let context = ClaimPoWRewardVerificationContext {
                    current_block_slot: helper.get_block_slot(),
                    reward_difficulty: helper.get_pow_reward_difficulty(),
                    pow_nullifiers: helper.get_pow_nullifiers(),
                    epoch_pow_reward: helper.get_epoch_pow_reward(),
                    epoch_reward_pool: helper.get_pow_reward_pool(),
                    current_epoch: helper.get_epoch(),
                    previous_epoch: helper.get_previous_epoch(),
                    blocks_slot: helper.get_blocks_slot(),
                };
                op.into_verified(&context)
                    .map(SignedOp::from)
                    .map_err(map_verify_failure)
            }
        }
    }
}

macro_rules! impl_from_signed_operation {
    ($($op:ty => $variant:ident),* $(,)?) => {
        $(
            impl<State: VerificationState, Mode: VerificationMode>
                From<SignedOperation<$op, State, Mode>> for SignedOp<State, Mode>
            {
                fn from(op: SignedOperation<$op, State, Mode>) -> Self {
                    Self::$variant(op)
                }
            }
        )*
    };
}

impl_from_signed_operation! {
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

macro_rules! impl_try_from_op_and_proof {
    ($($variant:ident => $op:ty),* $(,)?) => {
        impl<Mode: VerificationMode> TryFrom<(
            crate::mantle::ops::Op,
            crate::mantle::ops::OpProof
        )> for SignedOp<Unverified, Mode> {
            type Error = crate::mantle::ops::signed_op_error::OpProofMismatch;

            fn try_from((op, op_proof): (crate::mantle::ops::Op, crate::mantle::ops::OpProof)) -> Result<Self, Self::Error> {
                match op {
                    $(
                        crate::mantle::ops::Op::$variant(operation) => {
                            match <$op as crate::mantle::ledger::ProvableOperation>::Proof::try_from(op_proof) {
                                Ok(proof) => Ok(SignedOperation::new(operation, proof).into()),
                                Err(actual_proof) => Err(Self::Error::new(crate::mantle::ops::Op::$variant(operation), actual_proof)),
                            }
                        }
                    )*
                }
            }
        }
    };
}

impl_try_from_op_and_proof! {
    ChannelInscribe => InscriptionOp,
    ChannelConfig => ChannelConfigOp,
    ChannelDeposit => DepositOp,
    ChannelWithdraw => ChannelWithdrawOp,
    ChannelTransfer => ChannelTransferOp,
    SDPDeclare => SDPDeclareOp,
    SDPWithdraw => SDPWithdrawOp,
    SDPActive => SDPActiveOp,
    LeaderClaim => LeaderClaimOp,
    Transfer => TransferOp,
    ClaimPowReward => ClaimPowRewardOp,
}

#[cfg(test)]
mod tests {
    use super::{SignedOp, TransferOp};
    use crate::mantle::{
        Op, OpProof,
        ledger::verification_mode::StandardMode,
        ops::NoOpProof,
        transactions::states::Unverified,
    };

    type UnverifiedOp = SignedOp<Unverified, StandardMode>;

    #[test]
    fn try_from_mismatched_proof_returns_the_parts() {
        let op = Op::Transfer(TransferOp::sample());
        let proof = OpProof::None(NoOpProof);

        let Err(error) = UnverifiedOp::try_from((op.clone(), proof.clone())) else {
            panic!("{} accepted a {proof:?}", op.as_str());
        };

        assert_eq!(error.operation(), &op);
        assert_eq!(error.actual_proof(), &proof);
    }
}
