use key_management_system_service::keys::ZkPublicKey;
use nomos_core::{
    header::HeaderId,
    mantle::{Note, Utxo, Value, tx_builder::MantleTxBuilder},
};
use overwatch::{
    DynError,
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData, relay::OutboundRelay},
};
use tokio::sync::oneshot;

use crate::{WalletMsg, WalletService, WalletServiceSettings};

pub trait WalletServiceData:
    ServiceData<Settings = WalletServiceSettings, Message = WalletMsg>
{
    type Kms;
    type Cryptarchia;
    type Tx;
    type Storage;
}

impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> WalletServiceData
    for WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
{
    type Kms = Kms;
    type Cryptarchia = Cryptarchia;
    type Tx = Tx;
    type Storage = Storage;
}

pub struct WalletApi<Wallet, RuntimeServiceId>
where
    Wallet: WalletServiceData,
{
    relay: OutboundRelay<Wallet::Message>,
    _id: std::marker::PhantomData<RuntimeServiceId>,
}

impl<Wallet, RuntimeServiceId> WalletApi<Wallet, RuntimeServiceId>
where
    Wallet: WalletServiceData,
    RuntimeServiceId: AsServiceId<Wallet> + std::fmt::Debug + std::fmt::Display + Sync,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<Wallet::Message>) -> Self {
        Self {
            relay,
            _id: std::marker::PhantomData,
        }
    }

    #[must_use]
    pub async fn from_overwatch_handle(handle: &OverwatchHandle<RuntimeServiceId>) -> Self {
        let relay = handle.relay::<Wallet>().await.unwrap();
        Self::new(relay)
    }

    pub async fn get_balance(
        &self,
        tip: HeaderId,
        pk: ZkPublicKey,
    ) -> Result<Option<Value>, DynError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::GetBalance { tip, pk, resp_tx })
            .await
            .map_err(|e| format!("Failed to send balance request: {e:?}"))?;

        Ok(rx.await??)
    }

    pub async fn fund_and_sign_tx(
        &self,
        tip: HeaderId,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
    ) -> Result<nomos_core::mantle::SignedMantleTx, DynError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::FundAndSignTx {
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                resp_tx,
            })
            .await
            .map_err(|e| format!("Failed to send fund_and_sign_tx request: {e:?}"))?;

        Ok(rx.await??)
    }

    pub async fn transfer_funds(
        &self,
        tip: HeaderId,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        recipient_pk: ZkPublicKey,
        amount: Value,
    ) -> Result<nomos_core::mantle::SignedMantleTx, DynError> {
        let mantle_tx_builder =
            MantleTxBuilder::new().add_ledger_output(Note::new(amount, recipient_pk));
        self.fund_and_sign_tx(tip, mantle_tx_builder, change_pk, funding_pks)
            .await
    }

    pub async fn get_leader_aged_notes(&self, tip: HeaderId) -> Result<Vec<Utxo>, DynError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::GetLeaderAgedNotes { tip, resp_tx })
            .await
            .map_err(|e| format!("Failed to send get_leader_aged_notes request: {e:?}"))?;

        Ok(rx.await??)
    }
}
