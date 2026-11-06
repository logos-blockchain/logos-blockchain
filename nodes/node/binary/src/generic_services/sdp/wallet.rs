use lb_core::{
    mantle::{
        Note, Op, SignedOps,
        ledger::verification_mode::StandardMode,
        note_set::LEDGER_SET,
        notes::NoteNullifier,
        transactions::{MantleTxBuilder, builder::ZkTransferOpening, states::Preverified},
    },
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};
use lb_groth16::{Fr, fr_from_mod_bytes};
use lb_sdp_service::{
    DeclarationRequest, RuntimeDeclaration,
    wallet::{SdpWalletAdapter as SdpWalletAdapterTrait, SdpWalletConfig, SdpWalletError},
};
use lb_wallet_service::{
    TipResponse,
    api::{WalletApi, WalletServiceData, added_nullifiers},
};
use overwatch::{
    DynError,
    services::{AsServiceId, ServiceData, relay::OutboundRelay},
};

use crate::ValidatedTx;

pub struct SdpWalletAdapter<Service, RuntimeServiceId>
where
    Service: WalletServiceData,
{
    api: WalletApi<Service, RuntimeServiceId>,
}

impl<S, R> SdpWalletAdapter<S, R>
where
    S: WalletServiceData + Send + Sync,
    S::Message: Send,
    R: AsServiceId<S> + std::fmt::Debug + std::fmt::Display + Sync,
{
    // Rejects a funded transaction whose fee exceeds the configured maximum,
    // releasing `reserved`, the notes this request reserved for it.
    async fn check_tx_fee(
        &self,
        funded: &MantleTxBuilder,
        config: &SdpWalletConfig,
        reserved: Vec<NoteNullifier>,
    ) -> Result<(), SdpWalletError> {
        let checked = match funded.tx_fee() {
            Ok(tx_fee) if tx_fee > config.max_tx_fee => Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            }),
            Ok(_) => Ok(()),
            Err(error) => Err(error.into()),
        };
        if checked.is_err() {
            self.api.release_pending_notes(reserved).await;
        }
        checked
    }
}

#[async_trait::async_trait]
impl<S, R> SdpWalletAdapterTrait for SdpWalletAdapter<S, R>
where
    S: WalletServiceData + Send + Sync,
    S::Message: Send,
    R: AsServiceId<S> + std::fmt::Debug + std::fmt::Display + Sync,
{
    type WalletService = S;

    fn new(outbound_relay: OutboundRelay<<Self::WalletService as ServiceData>::Message>) -> Self {
        Self {
            api: WalletApi::new(outbound_relay),
        }
    }

    async fn declare_tx(
        &self,
        tx_builder: MantleTxBuilder,
        declaration: DeclarationRequest,
        config: &SdpWalletConfig,
    ) -> Result<SignedOps<Preverified, StandardMode>, SdpWalletError> {
        let TipResponse {
            tip: split_tip,
            response: (split_builder, exact),
        } = self
            .api
            .prepare_exact_note(
                None,
                tx_builder.clone(),
                declaration.amount,
                config.funding_pk,
                vec![config.funding_pk],
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;
        // The wallet holds the notes of the split until the transaction spends
        // them, so a declaration that fails before funding releases them.
        let split_notes = added_nullifiers(&tx_builder, &split_builder);
        let declared = async {
            let commitment_merkle_root =
                split_root(&split_builder).map_err(SdpWalletError::WalletApi)?;
            Ok(split_builder.clone().push_op_with_opening(
                Op::SDPDeclare(DeclarationMessage {
                    service_type: declaration.service_type,
                    locators: declaration.locators,
                    provider_id: declaration.provider_id,
                    zk_id: declaration.zk_id,
                    note_pk: declaration.note_pk,
                    note_stake_pk: declaration.note_stake_pk,
                    inputs: [exact.nullifier].into(),
                    commitment_merkle_root,
                    amount: declaration.amount,
                }),
                ZkTransferOpening {
                    inputs: vec![exact.note],
                    outputs: vec![],
                },
            )?)
        }
        .await;
        let tx_builder = match declared {
            Ok(tx_builder) => tx_builder,
            Err(error) => {
                self.api.release_pending_notes(split_notes).await;
                return Err(error);
            }
        };
        // A funding that fails releases the notes of `tx_builder` in the wallet.
        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx(
                // the split names the spend root of this tip
                Some(split_tip),
                tx_builder.clone(),
                config.funding_pk,
                vec![config.funding_pk],
                lb_wallet_service::FEE_MARGIN_PERCENT,
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let reserved = added_nullifiers(&tx_builder, &funded)
            .into_iter()
            .chain(split_notes)
            .collect();
        self.check_tx_fee(&funded, config, reserved).await?;

        let signed_tx = self
            .api
            .sign_tx(Some(tip), funded)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        Ok(signed_tx)
    }

    async fn withdraw_tx(
        &self,
        mut tx_builder: MantleTxBuilder,
        declaration: &RuntimeDeclaration,
        config: &SdpWalletConfig,
    ) -> Result<SignedOps<Preverified, StandardMode>, SdpWalletError> {
        let service_note = self
            .api
            .owned_note(None, declaration.service_note)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response
            .ok_or(SdpWalletError::ServiceNoteNotHeld(declaration.service_note))?;
        let output = Note::new(
            service_note.note.value,
            fr_from_mod_bytes(&rand::random::<[u8; 32]>()),
            config.funding_pk,
        );
        // the wallet adopts the withdrawn stake once its commitment is appended
        self.api
            .import_note(output, LEDGER_SET)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;
        tx_builder = tx_builder.push_op_with_opening(
            Op::SDPWithdraw(WithdrawMessage {
                declaration_id: declaration.id,
                service_note_nullifier: service_note.nullifier,
                outputs: [output.commitment()].into(),
                excess_value: 0,
            }),
            ZkTransferOpening {
                inputs: vec![service_note.note],
                outputs: vec![output],
            },
        )?;

        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx(
                None,
                tx_builder.clone(),
                config.funding_pk,
                vec![config.funding_pk],
                lb_wallet_service::FEE_MARGIN_PERCENT,
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let reserved = added_nullifiers(&tx_builder, &funded);
        self.check_tx_fee(&funded, config, reserved).await?;

        let signed_tx = self
            .api
            .sign_tx(Some(tip), funded)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        Ok(signed_tx)
    }

    async fn active_tx(
        &self,
        mut tx_builder: MantleTxBuilder,
        active: ActiveMessage,
        config: &SdpWalletConfig,
    ) -> Result<ValidatedTx, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPActive(active))?;

        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx(
                None,
                tx_builder.clone(),
                config.funding_pk,
                vec![config.funding_pk],
                lb_wallet_service::FEE_MARGIN_PERCENT,
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let reserved = added_nullifiers(&tx_builder, &funded);
        self.check_tx_fee(&funded, config, reserved).await?;

        let signed_tx = self
            .api
            .sign_tx(Some(tip), funded)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        Ok(signed_tx)
    }
}

// The ledger set root the exact note split, the last Operation of
// `tx_builder`, is proven against.
pub fn split_root(tx_builder: &MantleTxBuilder) -> Result<Fr, DynError> {
    match tx_builder.clone().build()?.last() {
        Some(Op::Transfer(split)) => Ok(split.commitment_merkle_root),
        _ => Err("the exact note split is missing".into()),
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{
        gas::GasCost, notes::NoteCommitment, ops::transfer::TransferOp,
        transactions::builder::FundingNote,
    };
    use lb_key_management_system_service::keys::ZkPublicKey;
    use lb_wallet_service::WalletMsg;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{RuntimeServiceId, generic_services};

    #[tokio::test]
    async fn a_fee_above_the_cap_releases_the_notes_of_the_funding_alone() {
        // the input of the caller's own op may be reserved by another request
        let unfunded = MantleTxBuilder::new()
            .push_op(Op::Transfer(TransferOp::new(
                [NoteNullifier(Fr::from(2u64))].into(),
                [NoteCommitment(Fr::from(1u64))].into(),
                Fr::from(5u64),
                0,
            )))
            .unwrap();
        let nullifier = NoteNullifier(Fr::from(3u64));
        let funded = unfunded
            .clone()
            .extend_funding_inputs(
                Fr::from(5u64),
                [FundingNote {
                    note: Note::new(10, Fr::from(1u64), ZkPublicKey::zero()),
                    nullifier,
                }],
            )
            .unwrap();
        let (msg_sender, mut msg_receiver) = mpsc::channel(1);
        let adapter =
            <generic_services::SdpWalletAdapter<RuntimeServiceId> as SdpWalletAdapterTrait>::new(
                OutboundRelay::new(msg_sender),
            );
        let config = SdpWalletConfig {
            max_tx_fee: GasCost::new(9),
            funding_pk: ZkPublicKey::zero(),
        };

        assert!(matches!(
            adapter
                .check_tx_fee(&funded, &config, added_nullifiers(&unfunded, &funded))
                .await,
            Err(SdpWalletError::TxFeeExceedsMaxFee { .. })
        ));
        assert!(matches!(
            msg_receiver.try_recv(),
            Ok(WalletMsg::ReleasePendingNotes { nullifiers }) if nullifiers == [nullifier]
        ));
    }
}
