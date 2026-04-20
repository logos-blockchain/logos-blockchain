use lb_core::{
    header::HeaderId,
    mantle::{
        Note, SignedMantleTx, TxHash, Value, ops::leader_claim::VoucherCm, tx::MantleTxContext,
        tx_builder::MantleTxBuilder,
    },
};
use lb_key_management_system_service::keys::{
    Ed25519Key, ZkPublicKey, ZkSignature, secured_key::SecuredKey,
};
use lb_wallet::WalletBalance;
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
        msg: Box<WalletMsg>,
    },
    #[error("Failed to recv message from wallet: {0}")]
    RelayRecv(#[from] RecvError),
    #[error(transparent)]
    Wallet(#[from] WalletServiceError),
}

impl From<(RelayError, WalletMsg)> for WalletApiError {
    fn from((relay_error, msg): (RelayError, WalletMsg)) -> Self {
        let msg = Box::new(msg);
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

    pub async fn get_known_addresses(&self) -> Result<Vec<ZkPublicKey>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::GetKnownAddresses { resp_tx })
            .await?;
        Ok(rx.await??)
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
    ) -> Result<TipResponse<Option<WalletBalance>>, WalletApiError> {
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

    pub async fn get_tx_context(
        &self,
        block_id: Option<HeaderId>,
    ) -> Result<MantleTxContext, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::GetTxContext { block_id, resp_tx })
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
        let context = self.get_tx_context(tip).await?;
        let mantle_tx_builder =
            MantleTxBuilder::new(context).add_ledger_output(Note::new(amount, recipient_pk));
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

    pub async fn sign_tx_with_ed25519(
        &self,
        tx_hash: TxHash,
        pk: <Ed25519Key as SecuredKey>::PublicKey,
    ) -> Result<<Ed25519Key as SecuredKey>::Signature, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::SignTxWithEd25519 {
                tx_hash,
                pk,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    pub async fn sign_tx_with_zk(
        &self,
        tx_hash: TxHash,
        pks: Vec<ZkPublicKey>,
    ) -> Result<ZkSignature, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::SignTxWithZk {
                tx_hash,
                pks,
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

#[cfg(test)]
mod tests {
    use std::fmt::{self, Display, Formatter};

    use lb_core::mantle::{
        ops::channel::{ChannelId, ChannelKeyIndex},
        tx::MantleTxGasContext,
    };
    use overwatch::services::state::{NoOperator, NoState};
    use tokio::sync::mpsc;

    use super::*;

    struct DummyWallet;

    impl ServiceData for DummyWallet {
        type Settings = WalletServiceSettings;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = WalletMsg;
    }

    impl WalletServiceData for DummyWallet {
        type Kms = ();
        type Cryptarchia = ();
        type Tx = ();
        type Storage = ();
    }

    #[derive(Debug)]
    struct TestRuntimeServiceId;

    impl AsServiceId<DummyWallet> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self;
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "TestRuntimeServiceId")
        }
    }

    #[tokio::test]
    async fn get_tx_context_round_trips_through_wallet_api() {
        let expected_block_id = HeaderId::from([7u8; 32]);
        let expected_channel_id = ChannelId::from([9u8; 32]);
        let expected_threshold: ChannelKeyIndex = 2;

        let (msg_sender, mut msg_receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            while let Some(msg) = msg_receiver.recv().await {
                if let WalletMsg::GetTxContext { block_id, resp_tx } = msg {
                    assert_eq!(block_id, Some(expected_block_id));
                    let context = MantleTxContext {
                        gas_context: MantleTxGasContext::new(
                            std::iter::once((expected_channel_id, expected_threshold)).collect(),
                        ),
                        leader_reward_amount: 0,
                    };
                    drop(resp_tx.send(Ok(context)));
                    break;
                }
            }
        });

        let api =
            WalletApi::<DummyWallet, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender));
        let context = api
            .get_tx_context(Some(expected_block_id))
            .await
            .expect("gas context should round-trip through the wallet API");

        assert_eq!(
            context.gas_context.withdraw_threshold(&expected_channel_id),
            Some(expected_threshold)
        );
        assert_eq!(
            context
                .gas_context
                .withdraw_threshold(&ChannelId::from([1u8; 32])),
            None
        );
        assert_eq!(context.leader_reward_amount, 0);
    }
}
