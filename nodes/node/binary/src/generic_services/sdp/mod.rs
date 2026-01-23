use lb_core::{
    mantle::{SignedMantleTx, tx_builder::MantleTxBuilder},
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};
use lb_sdp_service::wallet::{
    SdpWalletAdapter as SdpWalletAdapterTrait, SdpWalletConfig, SdpWalletError,
};
use overwatch::services::{ServiceData, relay::OutboundRelay};

pub struct SdpWalletAdapter<S, E>
where
    S: ServiceData,
{
    relay: OutboundRelay<S::Message>,
    _phantom: std::marker::PhantomData<E>,
}

#[async_trait::async_trait]
impl<S, E> SdpWalletAdapterTrait for SdpWalletAdapter<S, E>
where
    S: ServiceData + Send + Sync,
    S::Message: Send,
    E: Send + Sync,
{
    type WalletService = S;
    type WalletError = E;

    fn new(outbound_relay: OutboundRelay<<Self::WalletService as ServiceData>::Message>) -> Self {
        Self {
            relay: outbound_relay,
            _phantom: std::marker::PhantomData,
        }
    }

    async fn declare_tx(
        &self,
        tx_builder: MantleTxBuilder,
        declaration: DeclarationMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx, SdpWalletError<Self::WalletError>> {
        todo!(
            "Construct mantle transaction, request signing via relay, and validate fee against config.max_tx_fee"
        )
    }

    async fn active_tx(
        &self,
        tx_builder: MantleTxBuilder,
        active_message: ActiveMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx, SdpWalletError<Self::WalletError>> {
        todo!(
            "Construct activity mantle transaction, update nonce, and send signing request via relay"
        )
    }

    async fn withdraw_tx(
        &self,
        tx_builder: MantleTxBuilder,
        withdraw: WithdrawMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx, SdpWalletError<Self::WalletError>> {
        todo!("Construct withdrawal mantle transaction using locked_note_id and send via relay")
    }
}
