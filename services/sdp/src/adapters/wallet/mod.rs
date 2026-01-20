pub mod mock;
//pub mod service;

use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, tx_builder::MantleTxBuilder},
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};
use lb_key_management_system_keys::keys::ZkPublicKey;

#[async_trait::async_trait]
pub trait SdpWalletAdapter {
    type Error;
    type WalletApi;

    fn new(wallet_api: Self::WalletApi) -> Self;

    fn declare_tx(
        &self,
        tip: HeaderId,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        tx_builder: MantleTxBuilder,
        declaration: Box<DeclarationMessage>,
    ) -> Result<SignedMantleTx, Self::Error>;

    fn withdraw_tx(
        &self,
        tip: HeaderId,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        tx_builder: MantleTxBuilder,
        withdrawn_message: WithdrawMessage,
    ) -> Result<SignedMantleTx, Self::Error>;

    fn active_tx(
        &self,
        tip: HeaderId,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        tx_builder: MantleTxBuilder,
        active_message: ActiveMessage,
    ) -> Result<SignedMantleTx, Self::Error>;
}
