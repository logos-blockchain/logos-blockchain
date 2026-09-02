use lb_core::{
    mantle::{
        SignedMantleTx,
        channel::{SlotTimeframe, SlotTimeout},
        ledger::NoteId,
        ops::channel::{MsgId, config::Keys, inscribe::Inscription},
        transactions::{Ops, mantle_tx::RawMantleTx, states::Unverified},
    },
    proofs::channel_multi_sig_proof::IndexedSignature,
};
use lb_key_management_system_service::keys::Ed25519Signature;

use super::{
    types::{ChannelWalletView, Error, PreparedChannelConfig, WithdrawArg, WithdrawInputs},
    zone_sequencer::ZoneSequencer,
};
use crate::{adapter, sequencer::zone_sequencer::PublishReceipt};

/// Drive-loop handle for issuing commands to the sequencer.
///
/// Obtained via [`ZoneSequencer::handle`]. Because the handle holds a `&mut`
/// borrow of the sequencer, only the drive task can hold one — the borrow
/// checker enforces "drive-loop only," which removes any actor-vs-caller
/// deadlock window and lets every state-mutating method return the resulting
/// [`PublishReceipt`] inline so the caller can persist outbox + checkpoint
/// atomically.
///
/// Pattern:
/// ```ignore
/// loop {
///     tokio::select! {
///         Some(msg) = ui_rx.recv() => {
///             let publish_receipt = sequencer.handle().publish(msg)?;
///             db.tx(|t| {
///                 t.outbox(publish_receipt);
///                 t.save_checkpoint(publish_receipt.checkpoint());
///             });
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
    /// With funding configured ([`SequencerConfig::funding`]), first funds
    /// the transaction from the node's wallet (one HTTP round-trip) and
    /// signs the funded hash; a funding failure returns an error without
    /// mutating state. Then records the inscription as pending and queues a
    /// `post_transaction` future onto the drive loop's in-flight pool — the
    /// post itself happens asynchronously the next time the drive loop polls
    /// `next_event`. The returned [`PublishResult`] reflects this queued
    /// state, not a network acknowledgement; the tx may not have reached the
    /// node yet. The accompanying [`SequencerCheckpoint`] captures the new
    /// pending state so the caller can persist outbox + checkpoint
    /// atomically.
    ///
    /// Returns [`Error::Unavailable`] if cold-start backfill is still in
    /// progress (the sequencer hasn't emitted [`super::Event::Ready`] yet)
    /// — or, with funding configured, while the node is disconnected
    /// (funding needs the node; a fresh `Ready` event is emitted when the
    /// reconnect completes, signalling it is safe to retry). Fee-less
    /// sequencers keep the old contract: after the first `Ready`, publishes
    /// are always accepted — during a mid-life reconnect the tx is queued
    /// locally and posted when the stream resumes (or when our turn comes
    /// back). To wait for readiness asynchronously, subscribe via
    /// [`ZoneSequencer::subscribe_ready`].
    pub async fn publish(&mut self, data: Inscription) -> Result<PublishReceipt, Error> {
        self.sequencer.do_publish(data).await
    }

    /// Build a [`RawMantleTx`] for the given ops and an inscription message,
    /// without submitting it.
    ///
    /// The returned [`RawMantleTx`] should be signed by all parties and
    /// submitted via [`Self::submit_signed_tx`]. Does not mutate sequencer
    /// state.
    pub fn prepare_tx(
        &mut self,
        ops: Ops,
        data: Inscription,
    ) -> Result<(RawMantleTx, MsgId, Ed25519Signature), Error> {
        self.sequencer.do_prepare_tx(ops, data)
    }

    /// Sign a [`RawMantleTx`] using the sequencer's key.
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw). Does
    /// not mutate sequencer state.
    pub fn sign_tx(&mut self, tx: &RawMantleTx) -> Result<Ed25519Signature, Error> {
        self.sequencer.do_sign_tx(tx)
    }

    /// Enqueue a [`SignedMantleTx`] associated with a [`MsgId`] for posting.
    ///
    /// Synchronously records the tx as pending and queues a
    /// `post_transaction` future onto the drive loop's in-flight pool — the
    /// post runs the next time the drive loop polls `next_event`. The
    /// returned [`PublishReceipt`] reflects the queued state, not a network
    /// acknowledgement.
    pub fn submit_signed_tx(
        &mut self,
        tx: SignedMantleTx<Unverified>,
        msg_id: MsgId,
    ) -> Result<PublishReceipt, Error> {
        self.sequencer.do_submit_signed_tx(tx, msg_id)
    }

    /// Update the channel's config.
    ///
    /// For an existing channel the sequencer's signing key must be on the
    /// channel's current accredited list (at any position) and the channel's
    /// `configuration_threshold` must be 1 — this one-shot helper does not
    /// collect signatures from other key holders. Multi-sig channels are
    /// reconfigured by collecting the signatures out-of-band and submitting
    /// the fully-signed transaction via [`Self::submit_signed_tx`]. This
    /// overwrites the entire key list — include the sequencer's own key if
    /// it should remain authorized.
    ///
    /// `posting_timeframe` and `posting_timeout` control round-robin
    /// sequencer rotation (see Mantle spec). Pass `0` for both to keep a
    /// single fixed sequencer at index 0.
    ///
    /// With funding configured ([`SequencerConfig::funding`]), first funds
    /// the transaction from the node's wallet and signs the funded hash.
    /// Enqueues the config tx onto the drive loop's in-flight pool — the
    /// post runs the next time the drive loop polls `next_event`. The
    /// returned [`PublishResult`] reflects the queued state, not a network
    /// acknowledgement. The signed tx is also returned for callers that want
    /// to observe finalization via the event stream.
    pub async fn channel_config(
        &mut self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        transfer_threshold: u16,
    ) -> Result<(PublishReceipt, SignedMantleTx<Unverified>), Error> {
        self.sequencer
            .do_channel_config(
                keys,
                posting_timeframe,
                posting_timeout,
                configuration_threshold,
                transfer_threshold,
            )
            .await
    }

    /// Build and fund a channel-config tx for external multi-sig signing.
    ///
    /// The multi-sig counterpart of [`Self::channel_config`]: instead of
    /// signing with the sequencer's own key, it hands back a
    /// [`PreparedChannelConfig`] carrying the funded tx, the `sign_payload`
    /// each accredited key must sign, and the channel's current accredited
    /// keys / `configuration_threshold`. The caller collects a signature from
    /// each required key holder over `sign_payload`, then submits the
    /// fully-signed tx via [`Self::submit_channel_config`]. Does not mutate
    /// sequencer state.
    ///
    /// The config-lineage parent is auto-detected exactly as in
    /// [`Self::channel_config`], so the prepared config extends the current
    /// config tip. For an unclaimed channel the returned accredited-key list
    /// is empty and the threshold `0` — submit with no signatures.
    pub async fn prepare_channel_config(
        &mut self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        transfer_threshold: u16,
    ) -> Result<PreparedChannelConfig, Error> {
        self.sequencer
            .do_prepare_channel_config(
                keys,
                posting_timeframe,
                posting_timeout,
                configuration_threshold,
                transfer_threshold,
            )
            .await
    }

    /// Submit a [`PreparedChannelConfig`] with its externally-collected
    /// signatures.
    ///
    /// `signatures` must be indexed against
    /// [`PreparedChannelConfig::accredited_keys`] and strictly ascending by
    /// index. Assembles the fully-signed config tx and enqueues it for posting
    /// on the drive loop's in-flight pool — the returned [`PublishReceipt`]
    /// reflects the queued state, not a network acknowledgement.
    pub fn submit_channel_config(
        &mut self,
        prepared: PreparedChannelConfig,
        signatures: Vec<IndexedSignature>,
    ) -> Result<PublishReceipt, Error> {
        self.sequencer
            .do_submit_channel_config(prepared, signatures)
    }

    /// Publish an atomic inscription+withdraw bundle.
    ///
    /// Reads this sequencer's
    /// accredited-key index from cached channel state (kept fresh by the
    /// drive loop). Selects the inscription's `parent_msg` from the current
    /// canonical tip, builds the bundled `MantleTx` (funding it from the
    /// node's wallet when [`SequencerConfig::funding`] is set), signs the
    /// funded hash locally with the sequencer's key, and submits. Scoped to
    /// single-sequencer (centralized) channels — only the sequencer's own
    /// signature is used.
    ///
    /// Returns [`Error::Unavailable`] only if cold-start backfill is still
    /// in progress (see [`Self::publish`] for the latched readiness
    /// contract). After the first `Ready`, builds from cached channel state
    /// even mid-life reconnect and queues locally; the post fires once the
    /// stream resumes and our turn is current. Returns [`Error::Network`] if
    /// the channel's `transfer_threshold > 1` (which would require multi-sig
    /// orchestration this API doesn't support).
    ///
    /// `inputs` chooses which tracked channel notes fund the transfer —
    /// [`WithdrawInputs::Auto`] lets the SDK pick covering notes (largest
    /// first, own-key notes ahead of the rest), or [`WithdrawInputs::Explicit`]
    /// pins an exact input set.
    pub async fn publish_atomic_withdraw(
        &mut self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
        inputs: WithdrawInputs,
    ) -> Result<PublishReceipt, Error> {
        self.sequencer
            .do_publish_atomic_withdraw(inscribe, withdraws, inputs)
            .await
    }

    /// Pin an observed deposit without waiting for finalization: publish
    /// `[CHANNEL_INSCRIBE, CHANNEL_TRANSFER]`, the transfer consuming
    /// `consumed_notes` (the deposit's channel `NoteId`s from `DepositInfo`) so
    /// the tx lands only if the deposit is on chain. Same contract as
    /// [`Self::publish_atomic_withdraw`]; [`Error::Network`] if a note is not
    /// in the tracked set (deposit not on this branch, or already
    /// consumed).
    pub async fn publish_pin_deposit(
        &mut self,
        inscribe: Inscription,
        consumed_notes: Vec<NoteId>,
    ) -> Result<PublishReceipt, Error> {
        self.sequencer
            .do_publish_pin_deposit(inscribe, consumed_notes)
            .await
    }

    /// The channel's tracked note set — see [`ZoneSequencer::channel_wallet`].
    #[must_use]
    pub fn channel_wallet(&self) -> ChannelWalletView {
        self.sequencer.channel_wallet()
    }
}
