use super::{
    ChannelId, ChannelUpdateTx, Ed25519Key, Event, HashSet, Inscription, MsgId, NodeHttpClient,
    PolicyRuntime, SequencerChannelView, VecDeque, ZkPublicKey, ZoneNodeHttpClient, ZoneSequencer,
    build_funded_custom_tx, channel_inscriptions, finalized_inscriptions, runner,
    to_policy_runtime, warn,
};

pub struct CustomRepublishDeps {
    pub node_client: NodeHttpClient,
    pub channel_id: ChannelId,
    pub signing_key: Ed25519Key,
    pub funding_pk: ZkPublicKey,
    pub batches: VecDeque<Vec<Inscription>>,
}

pub fn start_custom_republish_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    deps: CustomRepublishDeps,
) -> PolicyRuntime {
    let view_rx = sequencer.subscribe_channel_view();
    let policy = CustomRepublishPolicy {
        deps,
        view_rx,
        pending: HashSet::new(),
        finalized: HashSet::new(),
        chain_tip: None,
        ready: false,
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

/// [`OrphanRepublishPolicy`] for the custom-tx flow: orphans that are
/// neither in `pending` nor finalized are rebuilt and re-submitted.
struct CustomRepublishPolicy {
    deps: CustomRepublishDeps,
    view_rx: tokio::sync::watch::Receiver<SequencerChannelView>,
    pending: HashSet<Inscription>,
    finalized: HashSet<Inscription>,
    /// Where our own submitted chain ends; reset on orphans so rebuilds
    /// chain from the channel tip instead.
    chain_tip: Option<MsgId>,
    /// No submissions until ready — a fail-fast submit would leak its
    /// funding reservation.
    ready: bool,
}

impl CustomRepublishPolicy {
    async fn submit<Node>(
        &mut self,
        sequencer: &mut ZoneSequencer<Node>,
        payloads: Vec<Inscription>,
    ) -> bool
    where
        Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
    {
        let parent = self
            .chain_tip
            .unwrap_or_else(|| self.view_rx.borrow().tip_message);
        let built = build_funded_custom_tx(
            &self.deps.node_client,
            self.deps.channel_id,
            &self.deps.signing_key,
            self.deps.funding_pk,
            &payloads,
            parent,
        )
        .await;
        let (signed_tx, msg_id) = match built {
            Ok(built) => built,
            Err(error) => {
                warn!(%error, "Failed to build custom zone tx");
                return false;
            }
        };
        match sequencer.handle().submit_signed_tx(signed_tx, msg_id) {
            Ok((_result, _checkpoint)) => {
                self.pending.extend(payloads);
                self.chain_tip = Some(msg_id);
                true
            }
            Err(error) => {
                warn!(%error, "Failed to submit custom zone tx");
                false
            }
        }
    }

    fn entry_payloads(&self, entry: &ChannelUpdateTx) -> Vec<Inscription> {
        match entry {
            ChannelUpdateTx::Custom(tx) => channel_inscriptions(tx, self.deps.channel_id)
                .into_iter()
                .map(|info| info.payload)
                .collect(),
            typed => typed
                .inscription()
                .map(|info| info.payload.clone())
                .into_iter()
                .collect(),
        }
    }
}

impl<Node> runner::Policy<Node> for CustomRepublishPolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let (channel_update, finalized) = match event {
            Event::Ready => {
                self.ready = true;
                (None, None)
            }
            Event::BlocksProcessed {
                channel_update,
                finalized,
                ..
            } => (Some(channel_update), Some(finalized)),
            _ => return,
        };

        if let Some(finalized) = finalized {
            self.finalized
                .extend(finalized_inscriptions(finalized).map(|info| info.payload.clone()));
        }

        if let Some(channel_update) = channel_update {
            let orphaned: HashSet<Inscription> = channel_update
                .orphaned
                .iter()
                .flat_map(|entry| self.entry_payloads(entry))
                .collect();
            let adopted: Vec<Inscription> = channel_update
                .adopted
                .iter()
                .flat_map(|entry| self.entry_payloads(entry))
                .collect();
            for payload in &orphaned {
                self.pending.remove(payload);
            }
            self.pending.extend(adopted);

            let republish: Vec<Inscription> = orphaned
                .into_iter()
                .filter(|payload| {
                    !self.pending.contains(payload) && !self.finalized.contains(payload)
                })
                .collect();
            if self.ready && !republish.is_empty() {
                self.chain_tip = None;
                if !self.submit(sequencer, republish.clone()).await {
                    self.deps.batches.push_back(republish);
                }
            }
        }

        // One attempt per batch per event: a failed submission stops the
        // drain and is retried on the next event.
        while self.ready {
            let Some(batch) = self.deps.batches.pop_front() else {
                break;
            };
            if !self.submit(sequencer, batch.clone()).await {
                self.deps.batches.push_front(batch);
                break;
            }
        }
    }
}
