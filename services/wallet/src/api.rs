use std::{collections::HashSet, mem};

use lb_chain_service::Epoch;
use lb_core::{
    header::HeaderId,
    mantle::{
        Note, SignedOps, Value,
        gas::GasCost,
        ledger::verification_mode::StandardMode,
        note_set::NoteSetIndex,
        notes::{NoteCommitment, NoteNullifier},
        ops::channel::ChannelId,
        transactions::{
            MantleTxBuilder, TxBuilderError, hash::TxHash, states::Preverified,
            tx_list::ops::OpsContext,
        },
    },
};
use lb_groth16::Fr;
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
use tokio::{
    runtime::Handle,
    sync::oneshot::{self, error::RecvError},
};

use crate::{
    AgedNote, ChannelStep, ChannelStepRequest, ChannelWithdrawal, LeaderAgedNotesInfo,
    ProvenFunding, ReservationHandoff, SignedTransaction, TipResponse, WalletMsg,
    WalletServiceError, WalletServiceSettings,
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

impl<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId> WalletServiceData
    for crate::WalletService<Kms, Cryptarchia, Tx, TimeBackend, RuntimeServiceId>
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

/// Releases acquired capabilities if the owning async operation is cancelled.
pub struct ReservationGuard {
    relay: OutboundRelay<WalletMsg>,
    reservation_ids: Vec<[u8; 32]>,
    runtime: Option<Handle>,
}

impl ReservationGuard {
    pub fn disarm(&mut self) {
        self.reservation_ids.clear();
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if self.reservation_ids.is_empty() {
            return;
        }
        let ids = mem::take(&mut self.reservation_ids);
        let relay = self.relay.clone();
        if let Some(runtime) = self.runtime.clone().or_else(|| Handle::try_current().ok()) {
            runtime.spawn(async move {
                for id in ids {
                    let (resp_tx, _rx) = oneshot::channel();
                    if let Err(err) = relay.send(WalletMsg::ReleaseReservation {
                        builder: MantleTxBuilder::new().with_reservation_id(id),
                        resp_tx,
                    }).await {
                        tracing::warn!(target: crate::LOG_TARGET, ?err, "Failed to release a cancelled reservation");
                    }
                }
            });
        }
    }
}

struct HandoffGuard {
    handoff: ReservationHandoff,
    reservation: ReservationGuard,
}

impl HandoffGuard {
    fn complete(&mut self) {
        self.handoff.complete();
        self.reservation.disarm();
    }
}

impl Drop for HandoffGuard {
    fn drop(&mut self) {
        if !self.handoff.cancel() {
            self.reservation.disarm();
        }
    }
}

/// The notes `after` spends that `before`, the builder it was built from, does
/// not: the inputs a split or a funding added.
#[must_use]
pub fn added_nullifiers(
    before: &MantleTxBuilder,
    after: &MantleTxBuilder,
) -> Vec<(NoteSetIndex, NoteNullifier)> {
    let before = before
        .note_set_nullifiers_consumed()
        .collect::<HashSet<_>>();
    after
        .note_set_nullifiers_consumed()
        .filter(|nullifier| !before.contains(nullifier))
        .collect()
}

impl<Wallet, RuntimeServiceId> WalletApi<Wallet, RuntimeServiceId>
where
    Wallet: WalletServiceData,
    RuntimeServiceId: AsServiceId<Wallet> + std::fmt::Debug + std::fmt::Display + Sync,
{
    /// Keep a reservation alive only while this operation remains in progress.
    #[must_use]
    pub fn guard_reservations(&self, builder: &MantleTxBuilder) -> ReservationGuard {
        ReservationGuard {
            relay: self.relay.clone(),
            reservation_ids: builder.reservation_ids().collect(),
            runtime: Handle::try_current().ok(),
        }
    }

    fn reservation_handoff(&self) -> HandoffGuard {
        let handoff = ReservationHandoff::new();
        let builder = MantleTxBuilder::new().with_reservation_id(handoff.owner);
        HandoffGuard {
            handoff,
            reservation: self.guard_reservations(&builder),
        }
    }
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
    /// final mandatory fee as a priority reserve, or the fee margin of the
    /// wallet configuration if `None`. The mandatory fee includes
    /// execution and storage cost; only the unused reserve becomes an
    /// effective priority tip. The percentage is not capped at 100. A
    /// `max_tx_fee` cuts the reserve down so the whole fee stays within it,
    /// never below the mandatory fee.
    pub async fn fund_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: Vec<ZkPublicKey>,
        priority_fee_percent: Option<u64>,
        max_tx_fee: Option<GasCost>,
    ) -> Result<TipResponse<MantleTxBuilder>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        let mut handoff = self.reservation_handoff();

        self.relay
            .send(WalletMsg::FundTx {
                handoff: handoff.handoff.clone(),
                tip,
                tx_builder,
                change_pk,
                funding_pks,
                priority_fee_percent,
                max_tx_fee,
                resp_tx,
            })
            .await?;

        let response = rx.await??;
        handoff.complete();
        Ok(response)
    }

    /// The built Operations and the proof of their funding Transfer, `None`
    /// when the builder does not fund them.
    pub async fn prove_funding(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
    ) -> Result<TipResponse<ProvenFunding>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        let mut handoff = self.reservation_handoff();

        self.relay
            .send(WalletMsg::ProveFunding {
                handoff: handoff.handoff.clone(),
                tip,
                tx_builder,
                resp_tx,
            })
            .await?;

        let response = rx.await??;
        handoff.complete();
        Ok(response)
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
        let mut handoff = self.reservation_handoff();

        self.relay
            .send(WalletMsg::PrepareExactNote {
                handoff: handoff.handoff.clone(),
                tip,
                tx_builder,
                value,
                owner,
                funding_pks,
                resp_tx,
            })
            .await?;

        let response = rx.await??;
        handoff.complete();
        Ok(response)
    }

    pub async fn owned_note(
        &self,
        tip: Option<HeaderId>,
        note_set: NoteSetIndex,
        commitment: NoteCommitment,
    ) -> Result<TipResponse<Option<OwnedNote>>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();

        self.relay
            .send(WalletMsg::OwnedNote {
                tip,
                note_set,
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

    // A leader reward note of `value` under `public_key`, with a private random
    // nonce, for a block about to be built. The wallet keeps it only once it is
    // passed to `track_leader_reward`.
    pub async fn new_leader_reward(
        &self,
        value: Value,
        public_key: ZkPublicKey,
    ) -> Result<Note, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::NewLeaderReward {
                value,
                public_key,
                resp_tx,
            })
            .await?;
        Ok(rx.await??)
    }

    // Keeps `note`, the leader reward note of a block about to be published,
    // returning once the persisted state of the wallet knows it.
    pub async fn track_leader_reward(&self, note: Note) -> Result<(), WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::TrackLeaderReward { note, resp_tx })
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

    /// A user step, or a program deposit, spending the channel notes of the
    /// wallet, with its proof.
    pub async fn prove_channel_step(
        &self,
        tip: Option<HeaderId>,
        request: ChannelStepRequest,
    ) -> Result<TipResponse<ChannelStep>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        let mut handoff = self.reservation_handoff();
        self.relay
            .send(WalletMsg::ProveChannelStep {
                handoff: handoff.handoff.clone(),
                tip,
                request,
                resp_tx,
            })
            .await?;
        let response = rx.await??;
        handoff.complete();
        Ok(response)
    }

    /// A withdrawal of the channel notes `notes` of the wallet, each released
    /// as a ledger note of the same value and key.
    pub async fn channel_withdrawal(
        &self,
        tip: Option<HeaderId>,
        channel_id: ChannelId,
        notes: Vec<NoteCommitment>,
    ) -> Result<TipResponse<ChannelWithdrawal>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        let mut handoff = self.reservation_handoff();
        self.relay
            .send(WalletMsg::ChannelWithdrawal {
                handoff: handoff.handoff.clone(),
                tip,
                channel_id,
                notes,
                resp_tx,
            })
            .await?;
        let response = rx.await??;
        handoff.complete();
        Ok(response)
    }

    /// The ledger set root every spend from `tip` names. See
    /// [`lb_wallet::Wallet::spend_root`].
    pub async fn spend_root(
        &self,
        tip: Option<HeaderId>,
    ) -> Result<TipResponse<Fr>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::SpendRoot { tip, resp_tx })
            .await?;
        Ok(rx.await??)
    }

    /// Releases the notes of `nullifiers`, which the wallet reserved for a
    /// split or a funding of a transaction that will not be submitted, so
    /// that they fund the next request. Only reservations owned by the
    /// builder's current claims are released. If the release does not reach
    /// the wallet, the notes stay reserved until their reservation expires.
    pub async fn release_pending_notes(
        &self,
        builder: &MantleTxBuilder,
        nullifiers: Vec<(NoteSetIndex, NoteNullifier)>,
    ) {
        if let Err(err) = self
            .relay
            .send(WalletMsg::ReleasePendingNotes {
                nullifiers,
                builder: builder.clone(),
            })
            .await
        {
            tracing::warn!(
                target: crate::LOG_TARGET,
                ?err,
                "Failed to release the pending notes of a transaction"
            );
        }
    }

    /// Request release of all inputs owned by a reservation capability.
    /// Returns whether that capability currently owns any input.
    pub async fn release_reservation(
        &self,
        reservation_id: [u8; 32],
    ) -> Result<bool, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        self.relay
            .send(WalletMsg::ReleaseReservation {
                builder: MantleTxBuilder::new().with_reservation_id(reservation_id),
                resp_tx,
            })
            .await?;
        Ok(rx.await?)
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
    ) -> Result<
        TipResponse<(SignedOps<Preverified, StandardMode>, Note, Vec<[u8; 32]>)>,
        WalletApiError,
    > {
        let note = Note::new(amount, crate::random_nonce(), recipient_pk);
        if self.get_known_addresses().await?.contains(&recipient_pk) {
            self.import_note(note, NoteSetIndex::Ledger).await?;
        }
        let mantle_tx_builder = MantleTxBuilder::new().add_ledger_output(note)?;
        let funded_tx_builder = self
            .fund_tx(tip, mantle_tx_builder, change_pk, funding_pks, None, None)
            .await?;
        let mut reservation_ids = funded_tx_builder
            .response
            .reservation_ids()
            .collect::<Vec<_>>();
        let mut reservation_guard = self.guard_reservations(&funded_tx_builder.response);
        let TipResponse { tip, response } = self
            .sign_tx(Some(funded_tx_builder.tip), funded_tx_builder.response)
            .await?;
        reservation_ids.extend(response.reservation_id);
        reservation_guard.disarm();
        Ok(TipResponse {
            tip,
            response: (response.transaction, note, reservation_ids),
        })
    }

    pub async fn sign_tx(
        &self,
        tip: Option<HeaderId>,
        tx_builder: MantleTxBuilder,
    ) -> Result<TipResponse<SignedTransaction>, WalletApiError> {
        let (resp_tx, rx) = oneshot::channel();
        let mut handoff = self.reservation_handoff();

        self.relay
            .send(WalletMsg::SignTx {
                handoff: handoff.handoff.clone(),
                tip,
                tx_builder,
                resp_tx,
            })
            .await?;

        let response = rx.await??;
        handoff.complete();
        Ok(response)
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

    /// Returns the wallet's notes that are unspent at `tip`, or at the current
    /// tip when `tip` is `None`, and in the aged snapshot of `epoch`, the epoch
    /// the caller leads in.
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
    use std::{
        fmt::{self, Display, Formatter},
        thread,
    };

    use lb_core::mantle::{
        ops::channel::ChannelKeyIndex,
        transactions::{GasPrices, tx_list::ops::OpsGasContext},
    };
    use overwatch::services::state::{NoOperator, NoState};
    use tokio::sync::mpsc;

    use super::*;
    use crate::tests::GAS_PROFILE;

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

    #[test]
    fn added_inputs_compare_note_sets_as_well_as_nullifiers() {
        use lb_core::mantle::{
            Op,
            notes::OutputCommitments,
            ops::{sdp::SDPWithdrawOp, transfer::TransferOp},
        };
        let nullifier = NoteNullifier(Fr::from(7u64));
        let before = MantleTxBuilder::new()
            .push_op(Op::SDPWithdraw(SDPWithdrawOp {
                service_note_nullifier: nullifier,
                ..SDPWithdrawOp::sample()
            }))
            .unwrap();
        let after = before
            .clone()
            .push_op(Op::Transfer(TransferOp::new(
                [nullifier].into(),
                OutputCommitments::default(),
                Fr::from(1u64),
                0,
            )))
            .unwrap();
        assert_eq!(
            added_nullifiers(&before, &after),
            vec![(NoteSetIndex::Ledger, nullifier)]
        );
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
                            GAS_PROFILE,
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

    #[tokio::test]
    async fn dropping_a_reservation_guard_requests_owned_cleanup() {
        let (msg_sender, mut msg_receiver) = mpsc::channel(1);
        let api =
            WalletApi::<DummyWallet, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender));
        let builder = MantleTxBuilder::new().with_reservation_id([7; 32]);
        drop(api.guard_reservations(&builder));
        let Some(WalletMsg::ReleaseReservation { builder, .. }) = msg_receiver.recv().await else {
            panic!("expected a reservation release request");
        };
        assert_eq!(builder.reservation_id(), Some([7; 32]));

        let builder = MantleTxBuilder::new().with_reservation_id([6; 32]);
        let guard = api.guard_reservations(&builder);
        thread::spawn(move || drop(guard)).join().unwrap();
        let Some(WalletMsg::ReleaseReservation { builder, .. }) = msg_receiver.recv().await else {
            panic!("expected cleanup from outside the runtime");
        };
        assert_eq!(builder.reservation_id(), Some([6; 32]));

        let builder = MantleTxBuilder::new().with_reservation_id([8; 32]);
        let mut guard = api.guard_reservations(&builder);
        guard.disarm();
        drop(guard);
        assert!(matches!(
            msg_receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn cancellation_after_a_response_is_queued_releases_its_reservation() {
        let (msg_sender, mut msg_receiver) = mpsc::channel(4);
        let api =
            WalletApi::<DummyWallet, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender));
        let key = ZkPublicKey::new(Fr::from(1u64));
        let task = tokio::spawn(async move {
            api.fund_tx(None, MantleTxBuilder::new(), key, vec![key], None, None)
                .await
        });
        let Some(WalletMsg::FundTx {
            handoff,
            tx_builder,
            resp_tx,
            ..
        }) = msg_receiver.recv().await
        else {
            panic!("expected funding");
        };
        assert!(handoff.acquired());
        resp_tx
            .send(Ok(TipResponse {
                tip: HeaderId::from([0; 32]),
                response: tx_builder.with_reservation_id(handoff.owner),
            }))
            .unwrap();
        task.abort();
        drop(task.await);
        let Some(WalletMsg::ReleaseReservation { builder, .. }) = msg_receiver.recv().await else {
            panic!("expected cancellation cleanup");
        };
        assert_eq!(builder.reservation_id(), Some(handoff.owner));
    }

    #[tokio::test]
    async fn cancelled_queued_signing_and_proving_handoffs_are_not_delivered() {
        use lb_core::{era::ForkDigest, mantle::transactions::OpProofs};

        for proving in [false, true] {
            for acquired in [false, true] {
                let (msg_sender, mut msg_receiver) = mpsc::channel(4);
                let api = WalletApi::<DummyWallet, TestRuntimeServiceId>::new(OutboundRelay::new(
                    msg_sender,
                ));
                let task = tokio::spawn(async move {
                    if proving {
                        drop(api.prove_funding(None, MantleTxBuilder::new()).await);
                    } else {
                        drop(api.sign_tx(None, MantleTxBuilder::new()).await);
                    }
                });
                let message = msg_receiver.recv().await.unwrap();
                let ops = MantleTxBuilder::new()
                    .build(ForkDigest::new([0x11; 32]))
                    .unwrap();
                let handoff = match message {
                    WalletMsg::SignTx {
                        handoff, resp_tx, ..
                    } => {
                        if acquired {
                            assert!(handoff.acquired());
                        }
                        let transaction = SignedOps::from_parts(ops, OpProofs::empty())
                            .unwrap()
                            .preverify()
                            .unwrap();
                        assert!(
                            resp_tx
                                .send(Ok(TipResponse {
                                    tip: HeaderId::from([0; 32]),
                                    response: SignedTransaction {
                                        transaction,
                                        reservation_id: acquired.then_some(handoff.owner)
                                    },
                                }))
                                .is_ok()
                        );
                        handoff
                    }
                    WalletMsg::ProveFunding {
                        handoff, resp_tx, ..
                    } => {
                        if acquired {
                            assert!(handoff.acquired());
                        }
                        assert!(
                            resp_tx
                                .send(Ok(TipResponse {
                                    tip: HeaderId::from([0; 32]),
                                    response: ProvenFunding {
                                        ops,
                                        proof: None,
                                        reservation_id: acquired.then_some(handoff.owner)
                                    },
                                }))
                                .is_ok()
                        );
                        handoff
                    }
                    _ => panic!("expected signing or proving"),
                };
                task.abort();
                drop(task.await);
                assert!(handoff.cancelled());
                if acquired {
                    let Some(WalletMsg::ReleaseReservation { builder, .. }) =
                        msg_receiver.recv().await
                    else {
                        panic!("expected newly acquired reservation cleanup");
                    };
                    assert_eq!(builder.reservation_id(), Some(handoff.owner));
                } else {
                    assert!(msg_receiver.try_recv().is_err());
                }
            }
        }
    }

    #[tokio::test]
    async fn cancellation_before_acquisition_cannot_claim_an_existing_reservation() {
        let (msg_sender, mut msg_receiver) = mpsc::channel(4);
        let api =
            WalletApi::<DummyWallet, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender));
        let key = ZkPublicKey::new(Fr::from(1u64));
        let task = tokio::spawn(async move {
            api.fund_tx(None, MantleTxBuilder::new(), key, vec![key], None, None)
                .await
        });
        let Some(WalletMsg::FundTx { handoff, .. }) = msg_receiver.recv().await else {
            panic!("expected funding");
        };
        task.abort();
        drop(task.await);
        assert!(!handoff.acquired());
        assert!(msg_receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancelling_transfer_during_signing_releases_the_funded_claim() {
        use std::time::Duration;

        use tokio::time::timeout;

        let (msg_sender, mut msg_receiver) = mpsc::channel(8);
        let api =
            WalletApi::<DummyWallet, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender));
        let key = ZkPublicKey::new(Fr::from(1u64));
        let task =
            tokio::spawn(async move { api.transfer_funds(None, key, vec![key], key, 1).await });
        let Some(WalletMsg::GetKnownAddresses { resp_tx }) = msg_receiver.recv().await else {
            panic!("expected addresses")
        };
        resp_tx.send(Ok(Vec::new())).unwrap();
        let Some(WalletMsg::FundTx {
            tx_builder,
            resp_tx,
            ..
        }) = msg_receiver.recv().await
        else {
            panic!("expected funding")
        };
        resp_tx
            .send(Ok(TipResponse {
                tip: HeaderId::from([0; 32]),
                response: tx_builder.with_reservation_id([9; 32]),
            }))
            .unwrap();
        let Some(WalletMsg::SignTx {
            resp_tx: pending_signing,
            ..
        }) = msg_receiver.recv().await
        else {
            panic!("expected signing")
        };
        task.abort();
        drop(task.await);
        let Some(WalletMsg::ReleaseReservation { builder, .. }) =
            timeout(Duration::from_secs(5), msg_receiver.recv())
                .await
                .unwrap()
        else {
            panic!("expected cancellation cleanup")
        };
        assert_eq!(builder.reservation_id(), Some([9; 32]));
        drop(pending_signing);
    }

    #[tokio::test]
    async fn transfer_funds_is_funded_with_the_configured_fee_margin() {
        let (msg_sender, mut msg_receiver) = mpsc::channel(1);
        let margin = tokio::spawn(async move {
            while let Some(msg) = msg_receiver.recv().await {
                match msg {
                    WalletMsg::GetKnownAddresses { resp_tx } => drop(resp_tx.send(Ok(Vec::new()))),
                    WalletMsg::FundTx {
                        priority_fee_percent,
                        ..
                    } => return priority_fee_percent,
                    _ => {}
                }
            }
            panic!("the transfer should be funded");
        });

        let api =
            WalletApi::<DummyWallet, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender));
        let key = ZkPublicKey::new(Fr::from(1u64));
        // The funding request is answered by no one, so the transfer fails.
        drop(api.transfer_funds(None, key, vec![key], key, 1).await);

        assert_eq!(margin.await.unwrap(), None);
    }
}
