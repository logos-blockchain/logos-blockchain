use lb_core::mantle::{
    MantleTx, SignedMantleTx, Transaction as _,
    channel::{SlotTimeframe, SlotTimeout},
    encoding::Ops,
    ops::{
        Op,
        channel::{
            MsgId,
            config::Keys,
            inscribe::{Inscription, InscriptionOp},
            withdraw::ChannelWithdrawOp,
        },
    },
};
use lb_key_management_system_service::keys::Ed25519Signature;
use tracing::{debug, info};

use super::{
    TARGET,
    tx_builder::{
        build_atomic_withdraw_ops_proofs, create_channel_config_tx, create_inscribe_tx,
        find_own_key_index, prepare_tx, sign_tx,
    },
    types::{
        AtomicWithdrawInfo, Error, InscriptionInfo, PendingTx, PublishResult, SequencerCheckpoint,
        TxStatus, WithdrawArg, WithdrawInfo,
    },
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
///         Some(ev) = sequencer.next_event() => {
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
        self.ensure_ready()?;

        let parent = self.compute_publish_parent();
        let (signed_tx, new_msg_id) = create_inscribe_tx(
            self.sequencer.channel_id,
            &self.sequencer.signing_key,
            data.clone(),
            parent,
        );
        let id = signed_tx.mantle_tx.hash();

        debug!(target: TARGET,
            "Prepared publish: payload={:?}, parent={}, msg_id={}, tx={}",
            String::from_utf8_lossy(&data),
            hex::encode(parent.as_ref()),
            hex::encode(new_msg_id.as_ref()),
            hex::encode(id.0),
        );

        let info = InscriptionInfo {
            tx_hash: id,
            parent_msg: parent,
            this_msg: new_msg_id,
            payload: data.clone(),
        };

        // Safe to unwrap — ensure_ready() guarantees state is initialized.
        let state = self.sequencer.state.as_mut().unwrap();
        state.submit_inscription(signed_tx.clone(), parent, new_msg_id, data);
        self.sequencer.last_msg_id = new_msg_id;
        self.sequencer
            .queue_tx_status(id, TxStatus::AcceptedLocally);

        // Queue the post into the in-flight batch pool only if it's our
        // turn — otherwise the tx sits in `state.pending` as `!posted` and
        // is drained by `submit_unposted_inscriptions` on the next turn-
        // change. Failure of the immediate post is handled the same way:
        // tx stays `!posted` and the `resubmit_interval` self-heal tick (or
        // the next turn-change) re-queues.
        if self.sequencer.can_publish_inscription_now() {
            self.sequencer.queue_publish_post(id, signed_tx);
        }

        self.sequencer.publish_channel_view();

        let checkpoint = self
            .sequencer
            .publish_checkpoint()
            .ok_or(Error::Unavailable {
                reason: "checkpoint unavailable",
            })?;

        Ok((
            PublishResult {
                tx: PendingTx::Inscription(info),
            },
            checkpoint,
        ))
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
        self.ensure_ready()?;
        Ok(prepare_tx(
            ops,
            self.sequencer.channel_id,
            &self.sequencer.signing_key,
            data,
            self.sequencer.last_msg_id,
        ))
    }

    /// Sign a [`MantleTx`] using the sequencer's key.
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw). Does
    /// not mutate sequencer state.
    pub fn sign_tx(&mut self, tx: &MantleTx) -> Result<Ed25519Signature, Error> {
        self.ensure_ready()?;
        Ok(sign_tx(tx.hash(), &self.sequencer.signing_key))
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
        self.ensure_ready()?;

        // Safe to unwrap — ensure_ready() guarantees state is initialized.
        let state = self.sequencer.state.as_mut().unwrap();
        let id = tx.mantle_tx.hash();
        state.submit_other(tx.clone());
        let parent_msg = self.sequencer.last_msg_id;
        self.sequencer.last_msg_id = msg_id;
        self.sequencer
            .queue_tx_status(id, TxStatus::AcceptedLocally);

        info!(target: TARGET,
            "Submitted tx including inscription {:?}",
            id
        );

        // Locate the inscription payload in the tx ops to populate
        // `PendingTx::Inscription`. Falls back to an empty payload if the
        // tx carries no inscription op for our channel (e.g. a pure deposit
        // or admin tx submitted via this entry point).
        let payload = tx
            .mantle_tx
            .ops()
            .iter()
            .find_map(|op| match op {
                Op::ChannelInscribe(i) if i.channel_id == self.sequencer.channel_id => {
                    Some(i.inscription.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| Inscription::new_unchecked(Vec::new()));

        // Queue the post into the in-flight batch pool; the drive loop will
        // drain it.
        self.sequencer.queue_publish_post(id, tx);

        let checkpoint = self
            .sequencer
            .publish_checkpoint()
            .ok_or(Error::Unavailable {
                reason: "checkpoint unavailable",
            })?;

        Ok((
            PublishResult {
                tx: PendingTx::Inscription(InscriptionInfo {
                    tx_hash: id,
                    parent_msg,
                    this_msg: msg_id,
                    payload,
                }),
            },
            checkpoint,
        ))
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
        self.ensure_ready()?;

        let signed_tx = create_channel_config_tx(
            self.sequencer.channel_id,
            &[&self.sequencer.signing_key],
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            withdraw_threshold,
        );
        let tx_hash = signed_tx.mantle_tx.hash();

        // Safe to unwrap — ensure_ready() guarantees state is initialized.
        let state = self.sequencer.state.as_mut().unwrap();
        state.submit_other(signed_tx.clone());
        self.sequencer
            .queue_tx_status(tx_hash, TxStatus::AcceptedLocally);

        info!(target: TARGET, "Submitted channel_config transaction {}", hex::encode(tx_hash.0));

        // Queue the post into the in-flight batch pool; the drive loop will
        // drain it.
        self.sequencer
            .queue_publish_post(tx_hash, signed_tx.clone());

        self.sequencer.publish_channel_view();

        let checkpoint = self
            .sequencer
            .publish_checkpoint()
            .ok_or(Error::Unavailable {
                reason: "checkpoint unavailable",
            })?;

        // Channel-config txs don't carry an inscription payload — surface
        // them as an inscription entry with an empty payload, sharing the
        // tx hash as `this_msg` so consumers can correlate via `tx_hash`.
        Ok((
            PublishResult {
                tx: PendingTx::Inscription(InscriptionInfo {
                    tx_hash,
                    parent_msg: self.sequencer.last_msg_id,
                    this_msg: self.sequencer.last_msg_id,
                    payload: Inscription::new_unchecked(Vec::new()),
                }),
            },
            checkpoint,
            signed_tx,
        ))
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
        self.ensure_ready()?;

        if withdraws.is_empty() {
            return Err(Error::Network(
                "publish_atomic_withdraw requires at least one withdraw".into(),
            ));
        }

        // Use the cached channel state kept fresh by the drive loop — see
        // `ensure_connected` for the staleness gate.
        let channel_state = self.sequencer.channel_state.as_ref().ok_or_else(|| {
            Error::Network(format!(
                "publish_atomic_withdraw requires channel state for {:?}",
                self.sequencer.channel_id
            ))
        })?;
        if channel_state.withdraw_threshold > 1 {
            return Err(Error::Network(format!(
                "publish_atomic_withdraw requires withdraw_threshold == 1, got {}",
                channel_state.withdraw_threshold
            )));
        }
        let own_key_index = find_own_key_index(channel_state, &self.sequencer.signing_key)?;
        let mut next_nonce = channel_state.withdrawal_nonce;

        let parent = self.compute_publish_parent();

        let mut ops: Vec<Op> = Vec::with_capacity(withdraws.len() + 1);
        let mut withdraw_ops = Vec::with_capacity(withdraws.len());
        for arg in withdraws {
            let op = ChannelWithdrawOp {
                channel_id: self.sequencer.channel_id,
                outputs: arg.outputs,
                withdraw_nonce: next_nonce,
            };
            withdraw_ops.push(op.clone());
            ops.push(Op::ChannelWithdraw(op));
            next_nonce = next_nonce
                .checked_add(1)
                .ok_or_else(|| Error::Network("withdraw nonce overflow".into()))?;
        }

        let inscription_op = InscriptionOp {
            channel_id: self.sequencer.channel_id,
            inscription: inscribe.clone(),
            parent,
            signer: self.sequencer.signing_key.public_key(),
        };
        let msg_id = inscription_op.id();
        ops.push(Op::ChannelInscribe(inscription_op));

        let tx = MantleTx(Ops::try_from(ops).map_err(|e| {
            Error::Network(format!("atomic withdraw bundle exceeds op limit: {e:?}"))
        })?);
        let own_sig = sign_tx(tx.hash(), &self.sequencer.signing_key);
        let ops_proofs = build_atomic_withdraw_ops_proofs(&tx, own_key_index, own_sig)?;
        let signed_tx = SignedMantleTx::new(tx, ops_proofs)
            .map_err(|e| Error::Network(format!("signed tx assembly failed: {e:?}")))?;

        let tx_hash = signed_tx.mantle_tx.hash();
        let withdraw_infos: Vec<WithdrawInfo> = withdraw_ops
            .into_iter()
            .map(|op| WithdrawInfo { tx_hash, op })
            .collect();

        // Safe to unwrap — ensure_ready() guarantees state is initialized.
        let state = self.sequencer.state.as_mut().unwrap();
        state.submit_atomic_withdraw(
            signed_tx.clone(),
            parent,
            msg_id,
            inscribe.clone(),
            withdraw_infos.clone(),
        );
        self.sequencer.last_msg_id = msg_id;
        self.sequencer
            .queue_tx_status(tx_hash, TxStatus::AcceptedLocally);

        // Queue the post only if it's our turn — see `publish` for the
        // turn-gate rationale.
        if self.sequencer.can_publish_inscription_now() {
            self.sequencer.queue_publish_post(tx_hash, signed_tx);
        }

        self.sequencer.publish_channel_view();

        let checkpoint = self
            .sequencer
            .publish_checkpoint()
            .ok_or(Error::Unavailable {
                reason: "checkpoint unavailable",
            })?;

        Ok((
            PublishResult {
                tx: PendingTx::AtomicWithdraw(AtomicWithdrawInfo {
                    tx_hash,
                    inscription: InscriptionInfo {
                        tx_hash,
                        parent_msg: parent,
                        this_msg: msg_id,
                        payload: inscribe,
                    },
                    withdraws: withdraw_infos,
                }),
            },
            checkpoint,
        ))
    }

    fn ensure_ready(&self) -> Result<(), Error> {
        if !self.sequencer.is_ready() {
            return Err(Error::Unavailable {
                reason: "sequencer not yet ready",
            });
        }
        if self.sequencer.state.is_none() {
            return Err(Error::Unavailable {
                reason: "sequencer state not initialized",
            });
        }
        Ok(())
    }

    fn compute_publish_parent(&self) -> MsgId {
        let state = self.sequencer.state.as_ref().unwrap();
        self.sequencer
            .current_tip
            .map_or(self.sequencer.last_msg_id, |tip| state.publish_parent(tip))
    }
}
