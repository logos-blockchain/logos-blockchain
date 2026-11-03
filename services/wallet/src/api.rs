use lb_chain_service::Epoch;
use lb_core::{
    header::HeaderId,
    mantle::{
        Note, OpProof, SignedOps, Value,
        ledger::verification_mode::StandardMode,
        note_set::{LEDGER_SET, NoteSetIndex},
        notes::NoteCommitment,
        transactions::{
            MantleTxBuilder, Ops, TxBuilderError, hash::TxHash, states::Preverified,
            tx_list::ops::OpsContext,
        },
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey, secured_key::SecuredKey};
use lb_utils::bounded::BoundedError;
use lb_wallet::{KnownNote, OwnedNote, WalletBalance};
use overwatch::{
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceData,
        relay::{OutboundRelay, OutboundRelayError},
    },
};
use tokio::sync::oneshot::{self, error::RecvError};

use crate::{
    AgedNote, LeaderAgedNotesInfo, TipResponse, WalletMsg, WalletServiceError,
    WalletServiceSettings,
};

#[derive(Debug, thiserror::Error)]
pub enum WalletApiError {
    #[error("Failed to relay message: {0}")]
    RelaySend(#[from] Box<OutboundRelayError<WalletMsg>>),
    #[error("Failed to recv message from wallet: {0}")]
    RelayRecv(#[from] RecvError),
    #[error(transparent)]
    Wallet(#[from] WalletServiceError),
    #[error(transparent)]
    TxBuilderError(#[from] TxBuilderError),
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
}

impl From<OutboundRelayError<WalletMsg>> for WalletApiError {
    fn from(value: OutboundRelayError<WalletMsg>) -> Self {
        Self::RelaySend(Box::new(value))
    }
}

pub trait WalletServiceData:
    ServiceData<Settings = WalletServiceSettings, Message = WalletMsg>
{
    type Kms;
    type Cryptarchia;
    type Tx;
}

impl<Kms, Cryptarchia, Tx, RuntimeServiceId> WalletServiceData
    for crate::WalletService<Kms, Cryptarchia, Tx, RuntimeServiceId>
{
    type Kms = Kms;
    type Cryptarchia = Cryptarchia;
    type Tx = Tx;
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

    /// Fund a transaction, reserving `priority_fee_percent` percent of its
    /// final mandatory fee as a priority reserve. The mandatory fee includes
    /// execution and storage cost; only the unused reserve becomes an
    /// effective priority tip. The percentage is not capped at 100.
    pub async fn fund_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        priority_fee_percent: u64,
    ) -> Result<TipResponse<MantleTxBuilder>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::FundTx {
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                priority_fee_percent,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    /// The built Operations and the proof of their funding Transfer, `None`
    /// when the builder does not fund them.
    pub async fn prove_funding(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
    ) -> Result<TipResponse<(Ops, Option<OpProof>)>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::ProveFunding {
                tip,
                tx_builder,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    /// Pushes a Transfer giving `owner` a note of exactly `value`, which a
    /// later Operation of the transaction can spend.
    pub async fn prepare_exact_note(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        value: Value,
        owner: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
    ) -> Result<TipResponse<(MantleTxBuilder, KnownNote)>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::PrepareExactNote {
                tip,
                tx_builder,
                value,
                owner,
                funding_pks,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    pub async fn owned_note(
        &self,
        tip: Option<HeaderId>,
        commitment: NoteCommitment,
    ) -> Result<TipResponse<Option<OwnedNote>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::OwnedNote {
                tip,
                commitment,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    pub async fn generate_reward_key(&self) -> Result<ZkPublicKey, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::GenerateRewardKey { resp_tx })
            .await?;
        Ok(rx.await??)
    }

    pub async fn import_note(
        &self,
        note: Note,
        note_set: NoteSetIndex,
    ) -> Result<KnownNote, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::ImportNote {
                note,
                note_set,
                resp_tx,
            })
            .await?;
        Ok(rx.await??)
    }

    pub async fn get_tx_context(
        &self,
        block_id: Option<HeaderId>,
    ) -> Result<OpsContext, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::GetTxContext { block_id, resp_tx })
            .await?;
        Ok(rx.await??)
    }

    /// The signed transfer of `amount` to `recipient_pk`, and the note it
    /// creates for the sender to hand over.
    pub async fn transfer_funds(
        &self,
        tip: Option<HeaderId>,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        recipient_pk: ZkPublicKey,
        amount: Value,
    ) -> Result<TipResponse<(SignedOps<Preverified, StandardMode>, Note)>, WalletApiError> {
        let note = Note::new(amount, crate::random_nonce(), recipient_pk);
        if self.get_known_addresses().await?.contains(&recipient_pk) {
            self.import_note(note, LEDGER_SET).await?;
        }
        let mantle_tx_builder = MantleTxBuilder::new().add_ledger_output(note)?;
        let funded_tx_builder = self
            .fund_tx(tip, mantle_tx_builder, change_pk, funding_pks, 0)
            .await?;
        let TipResponse { tip, response } = self
            .sign_tx(Some(funded_tx_builder.tip), funded_tx_builder.response)
            .await?;
        Ok(TipResponse {
            tip,
            response: (response, note),
        })
    }

    pub async fn sign_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
    ) -> Result<TipResponse<SignedOps<Preverified, StandardMode>>, WalletApiError> {
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

    pub async fn get_leader_aged_notes(
        &self,
        tip: Option<HeaderId>,
        epoch: Epoch,
    ) -> Result<TipResponse<Vec<AgedNote>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::GetLeaderAgedNotes {
                tip,
                epoch,
                resp_tx,
            })
            .await?;

        Ok(rx.await??)
    }

    /// Reports which of the wallet's notes are old enough to take part in the
    /// leadership lottery at `tip`, or at the current tip when `tip` is
    /// `None`, along with the total value they stake.
    ///
    /// Unlike [`Self::get_leader_aged_notes`], this does not expose the key
    /// ids, so it is the variant to use for external reporting (HTTP, FFI).
    pub async fn get_leader_aged_notes_info(
        &self,
        tip: Option<HeaderId>,
    ) -> Result<TipResponse<LeaderAgedNotesInfo>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::GetLeaderAgedNotesInfo { tip, resp_tx })
            .await?;

        Ok(rx.await??)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::{self, Display, Formatter};

    use lb_core::mantle::{
        ops::channel::{ChannelId, ChannelKeyIndex},
        transactions::{GasPrices, tx_list::ops::OpsGasContext},
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
                    let context = OpsContext {
                        gas_context: OpsGasContext::new(
                            std::iter::once((expected_channel_id, expected_threshold)).collect(),
                            expected_gas_prices,
                        ),
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
            context
                .gas_context
                .configuration_threshold(&expected_channel_id),
            Some(expected_threshold)
        );
        assert_eq!(
            context
                .gas_context
                .configuration_threshold(&ChannelId::from([1u8; 32])),
            None
        );
    }
}
