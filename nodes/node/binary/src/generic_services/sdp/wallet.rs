use lb_core::{
    mantle::{Op, SignedMantleTx, gas::MainnetGasConstants, tx_builder::MantleTxBuilder},
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};
use lb_sdp_service::wallet::{
    SdpWalletAdapter as SdpWalletAdapterTrait, SdpWalletConfig, SdpWalletError,
};
use lb_wallet_service::api::{WalletApi, WalletServiceData};
use overwatch::services::{AsServiceId, ServiceData, relay::OutboundRelay};

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
        declaration: DeclarationMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPDeclare(declaration));

        let funded = self
            .api
            .fund_tx(None, tx_builder, config.funding_pk, vec![config.funding_pk])
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        let tx_fee = funded.gas_cost::<MainnetGasConstants>()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

        let signed_tx = self
            .api
            .sign_tx(None, funded)
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
    ) -> Result<SignedMantleTx, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPWithdraw(withdraw));

        let funded = self
            .api
            .fund_tx(None, tx_builder, config.funding_pk, vec![config.funding_pk])
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        let tx_fee = funded.gas_cost::<MainnetGasConstants>()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

        let signed_tx = self
            .api
            .sign_tx(None, funded)
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
    ) -> Result<SignedMantleTx, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPActive(active));

        let funded = self
            .api
            .fund_tx(None, tx_builder, config.funding_pk, vec![config.funding_pk])
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        let tx_fee = funded.gas_cost::<MainnetGasConstants>()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

        let signed_tx = self
            .api
            .sign_tx(None, funded)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        Ok(signed_tx)
    }
}
