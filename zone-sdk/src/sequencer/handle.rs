use lb_core::mantle::{
    MantleTx, SignedMantleTx, Transaction as _,
    channel::{SlotTimeframe, SlotTimeout},
    encoding::Ops,
    ops::channel::{MsgId, config::Keys, inscribe::Inscription},
};
use lb_key_management_system_service::keys::Ed25519Signature;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};

use super::{
    TARGET,
    types::{Error, Event, PublishResult, WithdrawArg},
    zone_sequencer::ActorRequest,
};
use crate::adapter;

/// Handle for issuing commands to the sequencer from other tasks.
///
/// Cheap to clone; safe to share across tasks. Holds only the async command
/// surface — sync queries and watch subscriptions live on
/// [`ZoneSequencer`](super::ZoneSequencer) itself.
///
/// # Method shapes
///
/// - **Fire-and-forget** ([`Self::publish_message`],
///   [`Self::publish_atomic_withdraw`]): queue a request via mpsc and return as
///   soon as it's queued. Safe to call from anywhere, including inside the
///   drive task.
/// - **Reply-awaiting** ([`Self::prepare_tx`], [`Self::sign_tx`],
///   [`Self::submit_signed_tx`], [`Self::channel_config`]): queue a request and
///   await a reply from the drive loop. Calling these from *inside* the drive
///   task will deadlock — the drive loop produces the reply, but it's blocked
///   on you.
#[derive(Clone)]
pub struct SequencerHandle<Node> {
    request_tx: mpsc::Sender<ActorRequest>,
    node: Node,
    event_tx: broadcast::Sender<Event>,
    // Private: used only by the fire-and-forget publish methods for a
    // cheap synchronous "is the sequencer ready?" precondition that avoids
    // queueing a request the actor would reject. Not exposed.
    ready_rx: watch::Receiver<bool>,
}

impl<Node> SequencerHandle<Node> {
    pub(super) const fn new(
        request_tx: mpsc::Sender<ActorRequest>,
        node: Node,
        event_tx: broadcast::Sender<Event>,
        ready_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            request_tx,
            node,
            event_tx,
            ready_rx,
        }
    }
}

impl<Node> SequencerHandle<Node>
where
    Node: adapter::Node + Sync,
{
    /// Publish an inscription to the zone's channel.
    ///
    /// Fire-and-forget: the inscription is queued for processing by the
    /// sequencer's event loop. The result (inscription ID) is delivered via
    /// [`Event::Published`] once the tx is created and posted to the network.
    ///
    /// Returns [`Error::Unavailable`] if the sequencer is not ready (cold
    /// start before the first live block, or mid-reconnect after a stream
    /// drop). Consumers driving the event loop can wait for the next
    /// [`Event::Readiness`] with `ready: true` and retry. To wait
    /// asynchronously, subscribe via
    /// [`ZoneSequencer::subscribe_ready`](super::ZoneSequencer::subscribe_ready).
    pub async fn publish_message(&self, data: Inscription) -> Result<(), Error> {
        if !*self.ready_rx.borrow() {
            return Err(Error::Unavailable {
                reason: "sequencer not yet ready",
            });
        }
        self.request_tx
            .send(ActorRequest::PublishMessage { data })
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })
    }

    /// Build a [`MantleTx`] for the given ops and an inscription message,
    /// without submitting it.
    ///
    /// The returned [`MantleTx`] should be signed by all parties and submitted
    /// via [`Self::submit_signed_tx`].
    pub async fn prepare_tx(
        &self,
        ops: Ops,
        data: Inscription,
    ) -> Result<(MantleTx, MsgId, Ed25519Signature), Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::PrepareTx {
            ops,
            msg: data,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })?
    }

    /// Sign a [`MantleTx`] using the sequencer's key.
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw).
    pub async fn sign_tx(&self, tx: &MantleTx) -> Result<Ed25519Signature, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::SignTx {
            tx_hash: tx.hash(),
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        let result = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })??;

        Ok(result)
    }

    /// Submit a [`SignedMantleTx`] that is associated with a [`MsgId`]
    pub async fn submit_signed_tx(
        &self,
        tx: SignedMantleTx,
        msg_id: MsgId,
    ) -> Result<PublishResult, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::SubmitSignedTx {
            tx: tx.clone(),
            msg_id,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        let result = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })??;

        info!(target: TARGET,
            "Submitted tx including inscription {:?}",
            result.inscription_id
        );

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self.node.post_transaction(tx).await {
            warn!(target: TARGET, "Failed to post transaction: {e}");
        }

        Ok(result)
    }

    /// Update the channel's config.
    ///
    /// The sequencer's signing key must be the channel administrator
    /// (`keys[0]`). This overwrites the entire key list — include the admin
    /// key if it should remain authorized.
    ///
    /// `posting_timeframe` and `posting_timeout` control round-robin
    /// sequencer rotation (see Mantle spec). Pass `0` for both to keep a
    /// single fixed sequencer at index 0.
    ///
    /// Returns the publish result (with checkpoint) and a future that
    /// resolves when the transaction is finalized.
    pub async fn channel_config(
        &self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
    ) -> Result<(PublishResult, impl Future<Output = Result<(), Error>>), Error> {
        // Subscribe BEFORE submitting to avoid missing finalization events.
        let mut event_rx = self.event_tx.subscribe();

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::ChannelConfig {
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            withdraw_threshold,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })?;

        let (signed_tx, publish_result) = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "sequencer dropped reply",
        })??;

        let tx_hash = signed_tx.mantle_tx.hash();

        info!(target: TARGET, "Submitted channel_config transaction {}", hex::encode(tx_hash.0));

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self.node.post_transaction(signed_tx).await {
            warn!(target: TARGET, "Failed to post channel_config transaction: {e}");
        }

        let finalized = async move {
            loop {
                match event_rx.recv().await {
                    Ok(Event::TxsFinalized { ref items })
                        if items.iter().any(|i| i.tx_hash == tx_hash) =>
                    {
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(_) => {
                        return Err(Error::Unavailable {
                            reason: "sequencer stopped",
                        });
                    }
                }
            }
        };

        Ok((publish_result, finalized))
    }

    /// Publish an atomic inscription+withdraw bundle.
    ///
    /// The SDK queries channel state to fill withdraw nonces and locate its
    /// own accredited-key index, selects the inscription's `parent_msg` from
    /// the current canonical tip, builds the bundled `MantleTx`, signs locally
    /// with the sequencer's key, and submits. Scoped to single-sequencer
    /// (centralized) channels — only the sequencer's own signature is used.
    ///
    /// Fire-and-forget: the bundle is queued for processing by the sequencer's
    /// event loop. The result is delivered via
    /// [`Event::Published`] (`PublishedTx::AtomicWithdraw` variant). Safe to
    /// call from the drive task itself (e.g. an orphan re-publish handler)
    /// because it does not await an actor reply.
    ///
    /// Returns [`Error::Unavailable`] if the sequencer is not ready (cold
    /// start before the first live block, or mid-reconnect after a stream
    /// drop). Consumers driving the event loop can wait for the next
    /// [`Event::Readiness`] with `ready: true` and retry.
    pub async fn publish_atomic_withdraw(
        &self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    ) -> Result<(), Error> {
        if !*self.ready_rx.borrow() {
            return Err(Error::Unavailable {
                reason: "sequencer not yet ready",
            });
        }
        self.request_tx
            .send(ActorRequest::PublishAtomicWithdraw {
                inscribe,
                withdraws,
            })
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })
    }
}
