use lb_core::{
    mantle::{
        Op, SignedOps,
        ledger::verification_mode::StandardMode,
        transactions::{MantleTxBuilder, states::Preverified},
    },
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_sdp_service::wallet::{
    SdpWalletAdapter as SdpWalletAdapterTrait, SdpWalletConfig, SdpWalletError,
};
use lb_wallet_service::{
    TipResponse,
    api::{WalletApi, WalletServiceData},
};
use overwatch::services::{AsServiceId, ServiceData, relay::OutboundRelay};

pub struct SdpWalletAdapter<Service, RuntimeServiceId>
where
    Service: WalletServiceData,
{
    api: WalletApi<Service, RuntimeServiceId>,
}

impl<S, R> SdpWalletAdapter<S, R>
where
    S: WalletServiceData,
    R: AsServiceId<S> + std::fmt::Debug + std::fmt::Display + Sync,
{
    async fn funding_pk(&self, config: &SdpWalletConfig) -> Result<ZkPublicKey, SdpWalletError> {
        self.api
            .get_known_keys()
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .remove(&config.funding_key_id)
            .ok_or_else(|| SdpWalletError::UnknownFundingKey(config.funding_key_id.clone()))
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
        mut tx_builder: MantleTxBuilder,
        declaration: DeclarationMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedOps<Preverified, StandardMode>, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPDeclare(declaration))?;

        let funding_pk = self.funding_pk(config).await?;
        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx(None, tx_builder, funding_pk, vec![funding_pk], 0)
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
        withdraw: WithdrawMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedOps<Preverified, StandardMode>, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPWithdraw(withdraw))?;

        let funding_pk = self.funding_pk(config).await?;
        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx(None, tx_builder, funding_pk, vec![funding_pk], 0)
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
    ) -> Result<SignedOps<Preverified, StandardMode>, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPActive(active))?;

        let funding_pk = self.funding_pk(config).await?;
        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx(None, tx_builder, funding_pk, vec![funding_pk], 0)
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
