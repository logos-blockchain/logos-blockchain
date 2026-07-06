use lb_core::mantle::{
    MantleTx, SignedMantleTx,
    channel::{SlotTimeframe, SlotTimeout},
    ops::channel::{MsgId, config::Keys, inscribe::Inscription},
    transactions::Ops,
};
use lb_key_management_system_service::keys::Ed25519Signature;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::{
    types::{
        Error, Event, PublishResult, SequencerChannelView, SequencerCheckpoint, TurnNotification,
        TxStatusUpdate, WithdrawArg,
    },
    zone_sequencer::ActorRequest,
};

/// Cheap-to-clone client for driving the sequencer from any task.
///
/// Obtained via [`super::ZoneSequencer::client`]. Mirrors the full surface a
/// consumer needs from outside the drive loop:
///
/// - Publish/admin methods route commands through an internal channel to the
///   actor, which processes them inside [`super::ZoneSequencer::next_event`].
///   The drive loop must therefore be polled for these calls to make progress;
///   an unpolled sequencer leaves client `.await`s pending forever.
/// - Subscription methods vend receivers from senders the client owns — they
///   work regardless of whether the drive loop is being polled.
///
/// Cloning is cheap (channel-sender clones). Each clone can be moved into a
/// separate task.
#[derive(Clone)]
pub struct SequencerClient {
    request_tx: mpsc::UnboundedSender<ActorRequest>,
    event_tx: broadcast::Sender<Event>,
    ready_tx: watch::Sender<bool>,
    channel_view_tx: watch::Sender<SequencerChannelView>,
    turn_to_write_tx: watch::Sender<TurnNotification>,
    checkpoint_tx: watch::Sender<Option<SequencerCheckpoint>>,
    tx_status_tx: broadcast::Sender<TxStatusUpdate>,
}

impl SequencerClient {
    pub(super) const fn new(
        request_tx: mpsc::UnboundedSender<ActorRequest>,
        event_tx: broadcast::Sender<Event>,
        ready_tx: watch::Sender<bool>,
        channel_view_tx: watch::Sender<SequencerChannelView>,
        turn_to_write_tx: watch::Sender<TurnNotification>,
        checkpoint_tx: watch::Sender<Option<SequencerCheckpoint>>,
        tx_status_tx: broadcast::Sender<TxStatusUpdate>,
    ) -> Self {
        Self {
            request_tx,
            event_tx,
            ready_tx,
            channel_view_tx,
            turn_to_write_tx,
            checkpoint_tx,
            tx_status_tx,
        }
    }

    /// Enqueue an inscription onto the zone's channel.
    ///
    /// Async counterpart of [`super::SequencerHandle::publish`].
    pub async fn publish(
        &self,
        data: Inscription,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.send(ActorRequest::Publish { data, response_tx })?;
        Self::recv(response_rx).await?
    }

    /// Publish an atomic inscription+withdraw bundle.
    ///
    /// Async counterpart of
    /// [`super::SequencerHandle::publish_atomic_withdraw`].
    pub async fn publish_atomic_withdraw(
        &self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.send(ActorRequest::PublishAtomicWithdraw {
            inscribe,
            withdraws,
            response_tx,
        })?;
        Self::recv(response_rx).await?
    }

    /// Update the channel's config.
    ///
    /// Async counterpart of [`super::SequencerHandle::channel_config`].
    pub async fn channel_config(
        &self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
    ) -> Result<(PublishResult, SequencerCheckpoint, SignedMantleTx), Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.send(ActorRequest::ChannelConfig {
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            withdraw_threshold,
            response_tx,
        })?;
        Self::recv(response_rx).await?
    }

    /// Enqueue a pre-signed [`SignedMantleTx`] for posting.
    ///
    /// Async counterpart of [`super::SequencerHandle::submit_signed_tx`].
    pub async fn submit_signed_tx(
        &self,
        tx: SignedMantleTx,
        msg_id: MsgId,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.send(ActorRequest::SubmitSignedTx {
            tx,
            msg_id,
            response_tx,
        })?;
        Self::recv(response_rx).await?
    }

    /// Build a [`MantleTx`] for the given ops and an inscription message.
    ///
    /// Async counterpart of [`super::SequencerHandle::prepare_tx`].
    pub async fn prepare_tx(
        &self,
        ops: Ops,
        data: Inscription,
    ) -> Result<(MantleTx, MsgId, Ed25519Signature), Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.send(ActorRequest::PrepareTx {
            ops,
            data,
            response_tx,
        })?;
        Self::recv(response_rx).await?
    }

    /// Sign a [`MantleTx`] using the sequencer's key.
    ///
    /// Async counterpart of [`super::SequencerHandle::sign_tx`]. Clones `tx`
    /// internally so the call site can keep its borrow.
    pub async fn sign_tx(&self, tx: &MantleTx) -> Result<Ed25519Signature, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.send(ActorRequest::SignTx {
            tx: tx.clone(),
            response_tx,
        })?;
        Self::recv(response_rx).await?
    }

    /// Subscribe to the broadcast channel of events.
    ///
    /// Late subscribers see events emitted from this point on (not the full
    /// history).
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Subscribe to readiness. See
    /// [`super::ZoneSequencer::subscribe_ready`] for semantics.
    #[must_use]
    pub fn subscribe_ready(&self) -> watch::Receiver<bool> {
        let mut rx = self.ready_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to channel-view changes.
    #[must_use]
    pub fn subscribe_channel_view(&self) -> watch::Receiver<SequencerChannelView> {
        let mut rx = self.channel_view_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to turn-to-write changes.
    #[must_use]
    pub fn subscribe_turn_to_write(&self) -> watch::Receiver<TurnNotification> {
        let mut rx = self.turn_to_write_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to persistence checkpoint changes.
    #[must_use]
    pub fn subscribe_checkpoint(&self) -> watch::Receiver<Option<SequencerCheckpoint>> {
        let mut rx = self.checkpoint_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to tx-status changes.
    #[must_use]
    pub fn subscribe_tx_status(&self) -> broadcast::Receiver<TxStatusUpdate> {
        self.tx_status_tx.subscribe()
    }

    fn send(&self, request: ActorRequest) -> Result<(), Error> {
        self.request_tx
            .send(request)
            .map_err(|_| Error::Unavailable {
                reason: "sequencer actor not running",
            })
    }

    async fn recv<T>(response_rx: oneshot::Receiver<T>) -> Result<T, Error> {
        response_rx.await.map_err(|_| Error::Unavailable {
            reason: "sequencer actor dropped request",
        })
    }
}
