use lb_core::mantle::{
    MantleTx, SignedMantleTx,
    channel::{SlotTimeframe, SlotTimeout},
    ops::channel::{MsgId, config::Keys, inscribe::Inscription},
    transactions::Ops,
};
use lb_key_management_system_service::keys::Ed25519Signature;

use super::{
    types::{Error, PublishResult, SequencerCheckpoint, WithdrawArg},
    zone_sequencer::ZoneSequencer,
};
use crate::adapter;

/// Drive-loop handle for issuing commands to the sequencer.
///
/// Obtained via [`ZoneSequencer::handle`]. Because the handle holds a `&mut`
/// borrow of the sequencer, only the drive task can hold one — the borrow
/// checker enforces "drive-loop only," which removes any actor-vs-caller
/// deadlock window and lets every state-mutating method return the resulting
/// [`SequencerCheckpoint`] inline so the caller can persist outbox + checkpoint
/// atomically.
///
/// Pattern:
/// ```ignore
/// loop {
///     tokio::select! {
///         Some(msg) = ui_rx.recv() => {
///             let (result, cp) = sequencer.handle().publish(msg)?;
///             db.tx(|t| { t.outbox(result); t.save_checkpoint(cp); });
///         }
///         ev = sequencer.next_event() => {
///             handle_event(ev, &mut sequencer, &mut db).await;
///         }
///     }
/// }
/// ```
pub struct SequencerHandle<'a, Node> {
    sequencer: &'a mut ZoneSequencer<Node>,
}

impl<'a, Node> SequencerHandle<'a, Node> {
    pub(super) const fn new(sequencer: &'a mut ZoneSequencer<Node>) -> Self {
        Self { sequencer }
    }
}

impl<Node> SequencerHandle<'_, Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Enqueue an inscription onto the zone's channel.
    ///
    /// Synchronously mutates state to record the inscription as pending and
    /// queues a `post_transaction` future onto the drive loop's in-flight
    /// pool — the post itself happens asynchronously the next time the drive
    /// loop polls `next_event`. The returned [`PublishResult`] reflects this
    /// queued state, not a network acknowledgement; the tx may not have
    /// reached the node yet. The accompanying [`SequencerCheckpoint`]
    /// captures the new pending state so the caller can persist outbox +
    /// checkpoint atomically.
    ///
    /// Returns [`Error::Unavailable`] only if cold-start backfill is still
    /// in progress (the sequencer hasn't emitted [`super::Event::Ready`]
    /// yet). After the first `Ready`, publishes are always accepted:
    /// during a mid-life reconnect the tx is queued locally and posted
    /// when the stream resumes (or when our turn comes back). To wait for
    /// readiness asynchronously, subscribe via
    /// [`ZoneSequencer::subscribe_ready`].
    pub fn publish(
        &mut self,
        data: Inscription,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        self.sequencer.do_publish(data)
    }

    /// Build a [`MantleTx`] for the given ops and an inscription message,
    /// without submitting it.
    ///
    /// The returned [`MantleTx`] should be signed by all parties and submitted
    /// via [`Self::submit_signed_tx`]. Does not mutate sequencer state.
    pub fn prepare_tx(
        &mut self,
        ops: Ops,
        data: Inscription,
    ) -> Result<(MantleTx, MsgId, Ed25519Signature), Error> {
        self.sequencer.do_prepare_tx(ops, data)
    }

    /// Sign a [`MantleTx`] using the sequencer's key.
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw). Does
    /// not mutate sequencer state.
    pub fn sign_tx(&mut self, tx: &MantleTx) -> Result<Ed25519Signature, Error> {
        self.sequencer.do_sign_tx(tx)
    }

    /// Enqueue a [`SignedMantleTx`] associated with a [`MsgId`] for posting.
    ///
    /// Synchronously records the tx as pending and queues a
    /// `post_transaction` future onto the drive loop's in-flight pool — the
    /// post runs the next time the drive loop polls `next_event`. The
    /// returned [`PublishResult`] reflects the queued state, not a network
    /// acknowledgement.
    pub fn submit_signed_tx(
        &mut self,
        tx: SignedMantleTx,
        msg_id: MsgId,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        self.sequencer.do_submit_signed_tx(tx, msg_id)
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
    /// Enqueues the config tx onto the drive loop's in-flight pool — the
    /// post runs the next time the drive loop polls `next_event`. The
    /// returned [`PublishResult`] reflects the queued state, not a network
    /// acknowledgement. The signed tx is also returned for callers that want
    /// to observe finalization via the event stream.
    pub fn channel_config(
        &mut self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
    ) -> Result<(PublishResult, SequencerCheckpoint, SignedMantleTx), Error> {
        self.sequencer.do_channel_config(
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            withdraw_threshold,
        )
    }

    /// Publish an atomic inscription+withdraw bundle.
    ///
    /// Reads the current on-chain `withdraw_nonce` and this sequencer's
    /// accredited-key index from cached channel state (kept fresh by the
    /// drive loop). Selects the inscription's `parent_msg` from the current
    /// canonical tip, builds the bundled `MantleTx`, signs locally with the
    /// sequencer's key, and submits. Scoped to single-sequencer (centralized)
    /// channels — only the sequencer's own signature is used.
    ///
    /// Returns [`Error::Unavailable`] only if cold-start backfill is still
    /// in progress (see [`Self::publish`] for the latched readiness
    /// contract). After the first `Ready`, builds from cached channel state
    /// even mid-life reconnect and queues locally; the post fires once the
    /// stream resumes and our turn is current. Returns [`Error::Network`] if
    /// the channel's `withdraw_threshold > 1` (which would require multi-sig
    /// orchestration this API doesn't support).
    pub fn publish_atomic_withdraw(
        &mut self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        self.sequencer
            .do_publish_atomic_withdraw(inscribe, withdraws)
    }
}
