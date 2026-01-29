use lb_core::{
    mantle::{SignedMantleTx, Value, tx_builder::MantleTxBuilder},
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};

#[derive(Debug, thiserror::Error)]
pub enum SdpWalletError {
    #[error(transparent)]
    WalletApi(DynError),
    #[error("Transaction fee exceeded the configured max fee. tx_fee={tx_fee} > max_fee={max_fee}")]
    TxFeeExceedsMaxFee { max_fee: Value, tx_fee: Value },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SdpWalletConfig {
    // Hard cap on the transaction fee initiated by SDP.
    pub max_tx_fee: Value,

    // The key to use for paying SDP transaction fees.
    // Change notes will be returned to this same funding pk.
    pub funding_pk: ZkPublicKey,
}

#[async_trait::async_trait]
pub trait SdpWalletAdapter {
    type WalletService: ServiceData;

    fn new(outbound_relay: OutboundRelay<<Self::WalletService as ServiceData>::Message>) -> Self;

    async fn declare_tx(
        &self,
        tx_builder: MantleTxBuilder,
        declaration: DeclarationMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx, SdpWalletError>;

    async fn active_tx(
        &self,
        tx_builder: MantleTxBuilder,
        active_message: ActiveMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx, SdpWalletError>;

    async fn withdraw_tx(
        &self,
        tx_builder: MantleTxBuilder,
        withdraw: WithdrawMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx, SdpWalletError>;
}
