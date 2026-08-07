use lb_core::{
    header::HeaderId,
    mantle::{
        FeePolicy, Note, SignedMantleTx, Value,
        gas::GasCost,
        ops::leader_claim::{RewardsRoot, VoucherCm},
        transactions::{
            MantleTxBuilder, MantleTxContext, TxBuilderError, hash::TxHash, states::Preverified,
        },
    },
};
use lb_key_management_system_service::keys::{
    Ed25519Key, ZkPublicKey, ZkPublicKeys, ZkSignature, secured_key::SecuredKey,
};
use lb_storage_service::backends::StorageBackend;
use lb_utils::bounded::BoundedError;
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
    ClaimableVoucherInfo, FundedTx, TipResponse, UtxoWithKeyId, WalletMsg, WalletServiceError,
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
    #[error(transparent)]
    TxBuilderError(#[from] TxBuilderError),
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
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
where
    Storage: StorageBackend + Send + Sync + 'static,
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

impl<Wallet, RuntimeServiceId> Clone for WalletApi<Wallet, RuntimeServiceId>
where
    Wallet: WalletServiceData,
{
    fn clone(&self) -> Self {
        Self {
            relay: self.relay.clone(),
            _id: std::marker::PhantomData,
        }
    }
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

    /// Funds a builder using live gas prices.
    ///
    /// When supplied, `max_tx_fee` is enforced before any funding notes are
    /// reserved by the wallet service.
    pub async fn fund_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        priority_fee: Value,
        max_tx_fee: Option<GasCost>,
    ) -> Result<TipResponse<MantleTxBuilder>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::FundTx {
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                priority_fee,
                max_tx_fee,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    /// Funds a builder using projected mandatory prices and returns its quote.
    pub async fn fund_tx_with_policy(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        policy: FeePolicy,
        max_tx_fee: Option<GasCost>,
    ) -> Result<TipResponse<FundedTx>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::FundTxWithPolicy {
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                policy,
                max_tx_fee,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    pub async fn build_leader_claim_tx(
        &self,
        tip: HeaderId,
        rewards_root: RewardsRoot,
        reward_amount: Value,
        funding_pk: ZkPublicKey,
        max_tx_fee: GasCost,
    ) -> Result<TipResponse<SignedMantleTx<Preverified>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::BuildLeaderClaimTx {
                tip,
                rewards_root,
                reward_amount,
                funding_pk,
                max_tx_fee,
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

    /// Builds, funds, and signs a legacy live-price transfer.
    ///
    /// When supplied, `max_tx_fee` is enforced before any funding notes are
    /// reserved by the wallet service.
    pub async fn transfer_funds(
        &self,
        tip: Option<HeaderId>,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        recipient_pk: ZkPublicKey,
        amount: Value,
        max_tx_fee: Option<GasCost>,
    ) -> Result<TipResponse<SignedMantleTx<Preverified>>, WalletApiError> {
        let mantle_tx_builder =
            MantleTxBuilder::new().add_ledger_output(Note::new(amount, recipient_pk))?;
        let funded_tx_builder = self
            .fund_tx(
                tip,
                mantle_tx_builder,
                change_pk,
                funding_pks,
                0,
                max_tx_fee,
            )
            .await?;
        self.sign_tx(tip, funded_tx_builder.response).await
    }

    /// Builds, funds, and signs a transfer using a fee policy and returns its
    /// quote. The optional fee limit is enforced before funding notes are
    /// reserved by the wallet service.
    #[expect(
        clippy::too_many_arguments,
        reason = "The public wallet operation keeps its existing argument layout while adding the optional fee limit."
    )]
    pub async fn transfer_funds_with_policy(
        &self,
        tip: Option<HeaderId>,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        recipient_pk: ZkPublicKey,
        amount: Value,
        policy: FeePolicy,
        max_tx_fee: Option<GasCost>,
    ) -> Result<TipResponse<(SignedMantleTx<Preverified>, FundedTx)>, WalletApiError> {
        let mantle_tx_builder =
            MantleTxBuilder::new().add_ledger_output(Note::new(amount, recipient_pk))?;
        let funded = self
            .fund_tx_with_policy(
                tip,
                mantle_tx_builder,
                change_pk,
                funding_pks,
                policy,
                max_tx_fee,
            )
            .await?;
        let signed = self
            .sign_tx(Some(funded.tip), funded.response.tx_builder.clone())
            .await?;
        Ok(TipResponse {
            tip: signed.tip,
            response: (signed.response, funded.response),
        })
    }

    pub async fn sign_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
    ) -> Result<TipResponse<SignedMantleTx<Preverified>>, WalletApiError> {
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
        pks: impl IntoIterator<Item = ZkPublicKey>,
    ) -> Result<ZkSignature, WalletApiError> {
        let pks = ZkPublicKeys::try_from_iter(pks)?;
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
        Ok(rx.await??)
    }

    pub async fn get_claimable_vouchers(
        &self,
        tip: Option<HeaderId>,
    ) -> Result<TipResponse<Vec<ClaimableVoucherInfo>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::GetClaimableVouchers { tip, resp_tx })
            .await?;
        Ok(rx.await??)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::{self, Display, Formatter};

    use lb_core::mantle::{
        ops::channel::{ChannelId, ChannelKeyIndex},
        transactions::{GasPrices, MantleTxGasContext},
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
        let expected_gas_prices = GasPrices::new(3, 7);

        let (msg_sender, mut msg_receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            while let Some(msg) = msg_receiver.recv().await {
                if let WalletMsg::GetTxContext { block_id, resp_tx } = msg {
                    assert_eq!(block_id, Some(expected_block_id));
                    let context = MantleTxContext {
                        gas_context: MantleTxGasContext::new(
                            std::iter::once((expected_channel_id, expected_threshold)).collect(),
                            std::collections::HashMap::new(),
                            expected_gas_prices,
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
            context.gas_context.transfer_threshold(&expected_channel_id),
            Some(expected_threshold)
        );
        assert_eq!(
            context
                .gas_context
                .transfer_threshold(&ChannelId::from([1u8; 32])),
            None
        );
        assert_eq!(context.leader_reward_amount, 0);
    }
}
