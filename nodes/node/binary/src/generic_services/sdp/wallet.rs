use lb_core::{
    mantle::{
        Note, Op, SignedOps,
        ledger::verification_mode::StandardMode,
        note_set::LEDGER_SET,
        transactions::{MantleTxBuilder, builder::ZkTransferOpening, states::Preverified},
    },
    sdp::{ActiveMessage, DeclarationMessage, Nonce, WithdrawMessage},
};
use lb_groth16::{Fr, fr_from_mod_bytes};
use lb_sdp_service::{
    DeclarationRequest, RuntimeDeclaration,
    wallet::{SdpWalletAdapter as SdpWalletAdapterTrait, SdpWalletConfig, SdpWalletError},
};
use lb_wallet_service::{
    TipResponse,
    api::{WalletApi, WalletServiceData},
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
        mut tx_builder: MantleTxBuilder,
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
                tx_builder,
                declaration.amount,
                config.funding_pk,
                vec![config.funding_pk],
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;
        let commitment_merkle_root =
            split_root(&split_builder).map_err(SdpWalletError::WalletApi)?;
        tx_builder = split_builder.push_op_with_opening(
            Op::SDPDeclare(DeclarationMessage {
                service_type: declaration.service_type,
                locators: declaration.locators,
                provider_id: declaration.provider_id,
                zk_id: declaration.zk_id,
                note_pk: declaration.note_pk,
                inputs: [exact.nullifier].into(),
                commitment_merkle_root,
                amount: declaration.amount,
            }),
            ZkTransferOpening {
                inputs: vec![exact.note],
                outputs: vec![],
            },
        )?;

        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx(
                // the split is proven against the root of this tip
                Some(split_tip),
                tx_builder,
                config.funding_pk,
                vec![config.funding_pk],
                0,
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let tx_fee = funded.tx_fee()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

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
        nonce: Nonce,
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
                nonce,
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
                tx_builder,
                config.funding_pk,
                vec![config.funding_pk],
                0,
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let tx_fee = funded.tx_fee()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

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
                tx_builder,
                config.funding_pk,
                vec![config.funding_pk],
                0,
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let tx_fee = funded.tx_fee()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

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
