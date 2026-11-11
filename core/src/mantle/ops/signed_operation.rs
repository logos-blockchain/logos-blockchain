use std::marker::PhantomData;

use crate::{
    events::TxEvent,
    mantle::{
        GasProfile,
        batch::{DeferrableProof, DeferredProofs},
        gas::{Gas, OperationGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::VerificationMode,
        },
        transactions::states::{Preverified, Unverified, VerificationState, Verified},
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedOperation<T: ProvableOperation, State: VerificationState, Mode: VerificationMode> {
    operation: T,
    proof: T::Proof,
    _marker: PhantomData<(State, Mode)>,
}

impl<T: ProvableOperation, State: VerificationState, Mode: VerificationMode>
    SignedOperation<T, State, Mode>
{
    #[must_use]
    fn into_state<NewState: VerificationState>(self) -> SignedOperation<T, NewState, Mode> {
        let Self {
            operation,
            proof,
            _marker,
        } = self;

        SignedOperation::<T, NewState, Mode> {
            operation,
            proof,
            _marker: PhantomData,
        }
    }

    #[must_use]
    pub const fn operation(&self) -> &T {
        &self.operation
    }

    #[must_use]
    pub const fn proof(&self) -> &T::Proof {
        &self.proof
    }

    /// Converts a `SignedOperation<T, State, Mode>` into a
    /// `SignedOperation<T, NewState, Mode>` without performing any
    /// verification.
    #[cfg(not(any(test, feature = "test-utils")))]
    #[must_use]
    pub(crate) fn into_state_trusted<NewState: VerificationState>(
        self,
    ) -> SignedOperation<T, NewState, Mode> {
        self.into_state()
    }

    /// Converts a `SignedOperation<T, State, Mode>` into a
    /// `SignedOperation<T, NewState, Mode>` without performing any
    /// verification.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn into_state_trusted<NewState: VerificationState>(
        self,
    ) -> SignedOperation<T, NewState, Mode> {
        self.into_state()
    }
}

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Unverified, Mode> {
    #[must_use]
    pub const fn new(operation: T, proof: T::Proof) -> Self {
        Self {
            operation,
            proof,
            _marker: PhantomData,
        }
    }
}

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Unverified, Mode>
where
    Self: PreverifiableOperation<Mode>,
{
    pub fn into_preverified(
        self,
        context: &<Self as PreverifiableOperation<Mode>>::Context<'_>,
    ) -> Result<SignedOperation<T, Preverified, Mode>, <Self as PreverifiableOperation<Mode>>::Error>
    {
        self.preverify(context)?;
        Ok(self.into_state())
    }
}

pub type VerifyError<T, Mode> = (
    SignedOperation<T, Preverified, Mode>,
    <SignedOperation<T, Preverified, Mode> as VerifiableOperation<Mode>>::Error,
);

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Preverified, Mode>
where
    Self: VerifiableOperation<Mode, DeferredProof: DeferrableProof>,
{
    pub fn into_verified_deferred(
        self,
        context: &<Self as VerifiableOperation<Mode>>::Context<'_>,
        deferred_proofs: &mut DeferredProofs,
    ) -> Result<SignedOperation<T, Verified, Mode>, VerifyError<T, Mode>> {
        let verify_result = self.verify(context);
        match verify_result {
            Ok(deferred_proof) => {
                deferred_proof.defer_into(deferred_proofs);
                Ok(self.into_state())
            }
            Err(error) => Err((self, error)),
        }
    }
}

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Preverified, Mode>
where
    Self: VerifiableOperation<Mode, DeferredProof = ()>,
{
    pub fn into_verified(
        self,
        context: &<Self as VerifiableOperation<Mode>>::Context<'_>,
    ) -> Result<SignedOperation<T, Verified, Mode>, VerifyError<T, Mode>> {
        let verify_result = self.verify(context);
        match verify_result {
            Ok(()) => Ok(self.into_state()),
            Err(error) => Err((self, error)),
        }
    }
}

pub type ExecuteOk<'context, T, Mode> = (
    <SignedOperation<T, Verified, Mode> as ExecutableOperation>::Context<'context>,
    Vec<TxEvent>,
);
pub type ExecuteError<T, Mode> = (
    SignedOperation<T, Verified, Mode>,
    <SignedOperation<T, Verified, Mode> as ExecutableOperation>::Error,
);

impl<T: ProvableOperation, Mode: VerificationMode> SignedOperation<T, Verified, Mode>
where
    Self: ExecutableOperation,
{
    pub fn execute(
        self,
        context: <Self as ExecutableOperation>::Context<'_>,
    ) -> Result<ExecuteOk<'_, T, Mode>, ExecuteError<T, Mode>> {
        <Self as ExecutableOperation>::execute(&self, context).map_err(|error| (self, error))
    }
}

impl<Profile, T, State, Mode> OperationGas<Profile> for SignedOperation<T, State, Mode>
where
    Profile: GasProfile,
    T: OperationGas<Profile> + ProvableOperation,
    State: VerificationState,
    Mode: VerificationMode,
{
    const GAS_COST: Gas = T::GAS_COST;
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Slot;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature};

    use super::*;
    use crate::{
        mantle::{
            TxHash,
            channel::{Channels, Error},
            gas::MainnetGasProfile,
            ledger::verification_mode::StandardMode,
            note_set::{LEDGER_SET, NoteSets, TxNoteState},
            notes::{NoteNullifier, OutputCommitments},
            ops::{
                ChannelInscribeProof, ZkTransferProof,
                channel::{
                    ChannelId, MsgId, UnverifiedChannelKeys,
                    config::{ChannelConfigOp, ChannelConfigValidationContext},
                    inscribe::{InscriptionOp, InscriptionPreverificationContext},
                    withdraw::{ChannelWithdrawOp, PendingWithdrawals, WithdrawExecutionContext},
                },
                op_proof::samples::SampleProof as _,
                transfer::{TransferOp, TransferValidationContext},
            },
            transactions::hash::TxHashView,
        },
        proofs::channel_multi_sig_proof::ChannelMultiSigProof,
    };

    fn tx_hash_view() -> TxHashView {
        TxHashView::from(TxHash::from([31u8; 32]))
    }

    fn inscription(parent: MsgId) -> InscriptionOp {
        InscriptionOp {
            parent,
            ..InscriptionOp::sample()
        }
    }

    fn inscription_signature() -> ChannelInscribeProof {
        ChannelInscribeProof::without_steps(
            Ed25519Key::from_bytes(&[15; 32]).sign_payload(tx_hash_view().as_bytes().as_ref()),
        )
    }

    fn channel_id() -> ChannelId {
        ChannelId::from([29u8; 32])
    }

    fn verified_withdraw() -> SignedOperation<ChannelWithdrawOp, Verified, StandardMode> {
        SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelWithdrawOp {
                channel_id: channel_id(),
                inputs: [NoteNullifier(Fr::from(28u64))].into(),
                commitment_merkle_root: Fr::ZERO,
                outputs: OutputCommitments::default(),
            },
            ZkTransferProof::sample(),
        )
        .into_state_trusted()
    }

    #[test]
    fn into_preverified_preserves_the_operation_and_the_proof() {
        let operation = inscription(MsgId::root());
        let proof = inscription_signature();
        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation.clone(), proof.clone());

        let preverified = signed_operation
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .expect("the sample signer signed this transaction hash");

        assert_eq!(preverified.operation(), &operation);
        assert_eq!(preverified.proof(), &proof);
    }

    #[test]
    fn into_preverified_returns_the_error() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            inscription(MsgId::root()),
            ChannelInscribeProof::without_steps(Ed25519Signature::zero()),
        );

        let preverified_signed_operation =
            signed_operation.into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            });

        assert_eq!(preverified_signed_operation, Err(Error::InvalidSignature));
    }

    #[test]
    fn into_verified_preserves_the_operation_and_the_proof() {
        let operation = ChannelConfigOp::sample();
        let proof = ChannelMultiSigProof::empty();
        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation.clone(), proof.clone())
                .into_state_trusted::<Preverified>();
        let channels = Channels::new();

        let verified = signed_operation
            .into_verified(&ChannelConfigValidationContext {
                channels: &channels,
                tx_hash_view: &tx_hash_view(),
            })
            .expect("a configuration rooted at the genesis message opens a new channel");

        assert_eq!(verified.operation(), &operation);
        assert_eq!(verified.proof(), &proof);
    }

    #[test]
    fn into_verified_returns_the_operation_alongside_the_error() {
        let parent = MsgId::from([27u8; 32]);
        let sample = ChannelConfigOp::sample();
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelConfigOp::new(
                sample.channel(),
                parent,
                sample.keys().clone(),
                sample.posting_timeframe().clone(),
                sample.posting_timeout().clone(),
                sample.configuration_threshold(),
            )
            .unwrap(),
            ChannelMultiSigProof::empty(),
        )
        .into_state_trusted::<Preverified>();
        let channels = Channels::new();

        let verified_signed_operation =
            signed_operation
                .clone()
                .into_verified(&ChannelConfigValidationContext {
                    channels: &channels,
                    tx_hash_view: &tx_hash_view(),
                });

        let Err((returned, error)) = verified_signed_operation else {
            panic!("A configuration with an unknown parent fails verification.")
        };
        assert_eq!(signed_operation, returned);
        assert_eq!(
            error,
            Error::InvalidParent {
                channel_id: ChannelConfigOp::sample().channel(),
                parent: parent.into(),
                actual: MsgId::root().into(),
            }
        );
    }

    #[test]
    fn into_verified_defers_a_zk_proof_to_the_batch() {
        let note_sets = NoteSets::new();
        let root = note_sets
            .get(LEDGER_SET)
            .expect("the ledger note set always exists")
            .commitment_root();
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            TransferOp::new(
                [NoteNullifier(Fr::from(2u64))].into(),
                OutputCommitments::default(),
                root,
                0,
            ),
            ZkTransferProof::sample(),
        )
        .into_state_trusted::<Preverified>();

        let context = TransferValidationContext {
            note_sets: &note_sets,
            tx_notes: &TxNoteState::new(),
            block_number: 1,
            tx_hash_view: &tx_hash_view(),
        };

        let mut deferred_proofs = DeferredProofs::new();
        signed_operation
            .into_verified_deferred(&context, &mut deferred_proofs)
            .expect("the input is a ledger note no channel owns");

        assert_eq!(deferred_proofs.zk_transfers().len(), 1);
    }

    #[test]
    fn execute_returns_what_the_operation_produced() {
        let keys = UnverifiedChannelKeys::from(
            Ed25519Key::from_bytes(&[15; 32])
                .public_key()
                .into_unverified(),
        );
        let (channels, _) =
            Channels::new().create_channel(channel_id(), Slot::default(), keys, NoteSets::new());

        let (context, events) = verified_withdraw()
            .execute(WithdrawExecutionContext {
                channels: &channels,
                pending_withdrawals: PendingWithdrawals::new(),
                block_slot: Slot::default(),
            })
            .expect("the channel has a note set");

        assert_eq!(context.pending_withdrawals.iter().count(), 1);
        assert_eq!(events, vec![]);
    }

    #[test]
    fn execute_returns_the_operation_alongside_the_error() {
        let signed_operation = verified_withdraw();
        let channels = Channels::new();

        let execute_result = signed_operation.clone().execute(WithdrawExecutionContext {
            channels: &channels,
            pending_withdrawals: PendingWithdrawals::new(),
            block_slot: Slot::default(),
        });

        assert_eq!(
            execute_result,
            Err((
                signed_operation,
                Error::ChannelNotFound {
                    channel_id: channel_id()
                }
            ))
        );
    }

    #[test]
    fn gas_cost_forwards_the_operation_gas_cost() {
        assert_eq!(
            <SignedOperation<InscriptionOp, Unverified, StandardMode> as OperationGas<
                MainnetGasProfile,
            >>::GAS_COST,
            <InscriptionOp as OperationGas<MainnetGasProfile>>::GAS_COST
        );
    }
}
