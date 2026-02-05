use lb_core::{
    header::HeaderId,
    mantle::{
        Note, SignedMantleTx, Value, ops::leader_claim::VoucherCm, tx_builder::MantleTxBuilder,
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceData,
        relay::{OutboundRelay, RelayError},
    },
};
use tokio::sync::oneshot::{self, error::RecvError};

use crate::{
    TipResponse, UtxoWithKeyId, VoucherCommitmentAndNullifier, WalletMsg, WalletServiceError,
    WalletServiceSettings,
};

#[derive(Debug, thiserror::Error)]
pub enum WalletApiError {
    #[error("Failed to relay message with wallet:{relay_error:?}, msg={msg:?}")]
    RelaySend {
        relay_error: RelayError,
        msg: WalletMsg,
    },
    #[error("Failed to recv message from wallet: {0}")]
    RelayRecv(#[from] RecvError),
    #[error(transparent)]
    Wallet(#[from] WalletServiceError),
}

impl From<(RelayError, WalletMsg)> for WalletApiError {
    fn from((relay_error, msg): (RelayError, WalletMsg)) -> Self {
        Self::RelaySend { relay_error, msg }
    }
}

pub trait WalletServiceData:
    ServiceData<Settings = WalletServiceSettings, Message = WalletMsg>
{
    type Kms;
    type Cryptarchia;
    type Tx;
    type Storage;
}

impl<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId> WalletServiceData
    for crate::WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>
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
        tip: Option<HeaderId>,
        pk: ZkPublicKey,
    ) -> Result<TipResponse<Option<Value>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::GetBalance { tip, pk, resp_tx })
            .await?;

        Ok(rx.await??)
    }

    pub async fn fund_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
    ) -> Result<TipResponse<MantleTxBuilder>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::FundTx {
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    pub async fn transfer_funds(
        &self,
        tip: Option<HeaderId>,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        recipient_pk: ZkPublicKey,
        amount: Value,
    ) -> Result<TipResponse<SignedMantleTx>, WalletApiError> {
        let mantle_tx_builder =
            MantleTxBuilder::new().add_ledger_output(Note::new(amount, recipient_pk));
        let funded_tx_builder = self
            .fund_tx(tip, mantle_tx_builder, change_pk, funding_pks)
            .await?;
        self.sign_tx(tip, funded_tx_builder.response).await
    }

    pub async fn sign_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
    ) -> Result<TipResponse<SignedMantleTx>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::SignTx {
                tip,
                tx_builder,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    pub async fn get_leader_aged_notes(
        &self,
        tip: Option<HeaderId>,
    ) -> Result<TipResponse<Vec<UtxoWithKeyId>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::GetLeaderAgedNotes { tip, resp_tx })
            .await?;

        Ok(rx.await??)
    }

    pub async fn generate_new_voucher(&self) -> Result<VoucherCm, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::GenerateNewVoucherSecret { resp_tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn get_claimable_voucher(
        &self,
        tip: Option<HeaderId>,
    ) -> Result<TipResponse<Option<VoucherCommitmentAndNullifier>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::GetClaimableVoucher { tip, resp_tx })
            .await?;
        Ok(rx.await??)
    }
}
