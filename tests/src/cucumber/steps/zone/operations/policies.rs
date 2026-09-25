use super::{
    Arc, BTreeSet, ChannelUpdate, ChannelUpdateTx, DiscardedPayloads, Event, FinalizedTx, HashMap,
    HashSet, Inscription, InscriptionInfo, Inscriptions as _, LazyLock, MsgId, PolicyRuntime,
    SequencerChannelView, VecDeque, ZoneAccountBalances, ZoneNodeHttpClient, ZoneSequencer,
    contributed, finalized_inscriptions, parse_balance_payload, runner, to_policy_runtime, warn,
};

/// Spawn a sequencer drive task with a no-op policy. Step bodies drive
/// publishes via [`SequencerClient`]; events flow to `PolicyRuntime.events`.
/// If `republish_orphans` is set, the [`OrphanRepublishPolicy`] runs inline
/// inside the drive loop.
pub fn start_sequencer_event_loop(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    republish_orphans: bool,
) -> PolicyRuntime {
    if republish_orphans {
        to_policy_runtime(runner::spawn(sequencer, OrphanRepublishPolicy::default()))
    } else {
        to_policy_runtime(runner::spawn(sequencer, runner::PassivePolicy))
    }
}

/// Drives a competing-sequencer policy that publishes `planned` once ready and
/// re-publishes its own orphans (tracked by intent lineage) until they land —
/// correct even when payloads repeat.
pub fn start_republish_lineage_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    planned: Vec<Inscription>,
) -> PolicyRuntime {
    let policy = RepublishLineagePolicy {
        planned,
        published_initial: false,
        lineage: LineageTracker::default(),
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

/// Drives a policy that republishes orphaned balance updates only when the
/// local canonical view can still apply the update without going negative,
/// and lays planned balance updates whenever it's our turn to write.
pub fn start_balance_aware_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    initial_balances: ZoneAccountBalances,
    planned_payloads: Vec<Inscription>,
) -> PolicyRuntime {
    let view_rx = sequencer.subscribe_channel_view();
    let policy = BalanceAwarePolicy {
        balances: BalanceAwareState::new(initial_balances),
        planned: VecDeque::from(planned_payloads),
        view_rx,
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

/// Drives a deterministic conflict policy used by tests that expect the final
/// zone chain to converge to sorted payload order.
pub fn start_sorted_conflict_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    discarded: &DiscardedPayloads,
) -> PolicyRuntime {
    let policy = SortedConflictPolicy {
        state: SortedConflictState::new(Arc::clone(discarded)),
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

/// Inline policy: republish orphaned inscriptions not already back on the
/// canonical chain. Plain inscriptions only — bundles re-prepare themselves.
/// Assumes unique payloads; for repeating payloads see
/// [`RepublishLineagePolicy`].
///
/// The non-finalized view is read from each update (`common_prefix ++
/// adopted`), so a payload still on chain — a live twin of a dead one — is
/// never re-homed.
#[derive(Default)]
struct OrphanRepublishPolicy {
    /// Finalized payloads: permanently on chain, never orphaned.
    finalized: HashSet<Inscription>,
}

impl<Node> runner::Policy<Node> for OrphanRepublishPolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let Event::BlocksProcessed {
            channel_update,
            finalized,
            ..
        } = event
        else {
            return;
        };
        self.finalized
            .extend(finalized_inscriptions(finalized).map(|info| info.payload.clone()));
        let Some(chain) = channel_update.canonical_chain() else {
            return;
        };
        let on_chain: HashSet<&Inscription> =
            chain.inscriptions().map(|info| &info.payload).collect();
        for entry in channel_update.orphaned() {
            let ChannelUpdateTx::Inscription(info) = entry else {
                continue;
            };
            if on_chain.contains(&info.payload) || self.finalized.contains(&info.payload) {
                continue;
            }
            if let Err(error) = sequencer.handle().publish(info.payload.clone()).await {
                warn!(%error, "Failed to re-publish orphaned zone payload");
            }
        }
    }
}

/// Tracks our published inscriptions by intent lineage, so republishing works
/// even when payloads repeat (identical bytes published as distinct messages).
///
/// Each original publish is its own intent, rooted at its `this_msg`; every
/// republish we issue for an orphaned member is recorded under the same root.
/// An intent is "live" while any of its `this_msg`s is on the channel
/// (`adopted`) or in flight as a publish/republish we issued. Identical
/// payloads form distinct intents (distinct `this_msg`s), so each lands once,
/// and other sequencers' inscriptions are never in our map, so we never
/// republish theirs.
#[derive(Default)]
struct LineageTracker {
    /// Every `this_msg` we've published (originals + republishes) → intent
    /// root.
    intent_root: HashMap<MsgId, MsgId>,
    /// Per intent root, the `this_msg`s in the non-finalized view, plus those
    /// published since the last event.
    pending: HashMap<MsgId, HashSet<MsgId>>,
    /// Intent roots that have finalized — permanently landed, so the intent is
    /// considered live forever and never re-homed again.
    finalized_roots: HashSet<MsgId>,
}

impl LineageTracker {
    /// Record an original publish as its own intent, in flight.
    fn record_publish(&mut self, this_msg: MsgId) {
        self.intent_root.insert(this_msg, this_msg);
        self.pending.entry(this_msg).or_default().insert(this_msg);
    }

    /// Record a republish of `orphan` as a new live member of its intent.
    fn record_republish(&mut self, orphan: MsgId, republished: MsgId) {
        let root = self.intent_root.get(&orphan).copied().unwrap_or(orphan);
        self.intent_root.insert(republished, root);
        self.pending.entry(root).or_default().insert(republished);
    }

    /// Refresh per-intent liveness. On a conflict our members in
    /// `common_prefix ++ adopted` are the live ones; a republish issued last
    /// event is in the prefix as pending, so nothing carries over. On an
    /// extension nothing of ours leaves, and our own publishes never appear
    /// in `adopted`, so there is nothing to fold.
    fn observe(&mut self, channel_update: &ChannelUpdate) {
        let Some(chain) = channel_update.canonical_chain() else {
            return;
        };
        self.pending.clear();
        for info in chain.inscriptions() {
            if let Some(&root) = self.intent_root.get(&info.this_msg) {
                self.pending.entry(root).or_default().insert(info.this_msg);
            }
        }
    }

    /// Pin the intents of any finalized `this_msg`s of ours as permanently
    /// live — once a member finalizes the payload is on chain for good.
    fn observe_finalized(&mut self, finalized: impl Iterator<Item = MsgId>) {
        for this_msg in finalized {
            if let Some(&root) = self.intent_root.get(&this_msg) {
                self.finalized_roots.insert(root);
            }
        }
    }

    /// True if `this_msg` is one of ours.
    fn is_ours(&self, this_msg: &MsgId) -> bool {
        self.intent_root.contains_key(this_msg)
    }

    /// True if the intent of `this_msg` has finalized, or still has a live
    /// member.
    fn intent_live(&self, this_msg: &MsgId) -> bool {
        let root = self.intent_root.get(this_msg).copied().unwrap_or(*this_msg);
        self.finalized_roots.contains(&root)
            || self
                .pending
                .get(&root)
                .is_some_and(|members| !members.is_empty())
    }
}

/// Inline republish policy for channels whose payloads can repeat. Publishes
/// its own `planned` payloads once the sequencer is ready, then republishes any
/// of *our* orphans whose intent has no live member, tracking msg-id lineage
/// (the payload can't identify the message when it repeats). Owning the
/// publishes is what gives the policy its outbox: every `this_msg` it sends is
/// recorded.
struct RepublishLineagePolicy {
    planned: Vec<Inscription>,
    published_initial: bool,
    lineage: LineageTracker,
}

impl<Node> runner::Policy<Node> for RepublishLineagePolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        match event {
            Event::Ready if !self.published_initial => {
                self.published_initial = true;
                for payload in self.planned.clone() {
                    match sequencer.handle().publish(payload).await {
                        Ok((result, _checkpoint)) => {
                            self.lineage
                                .record_publish(result.tx.inscription().this_msg);
                        }
                        Err(error) => warn!(%error, "Failed to publish planned zone payload"),
                    }
                }
            }
            Event::BlocksProcessed {
                channel_update,
                finalized,
                ..
            } => {
                self.lineage
                    .observe_finalized(finalized_inscriptions(finalized).map(|i| i.this_msg));
                self.lineage.observe(channel_update);
                for entry in channel_update.orphaned() {
                    let ChannelUpdateTx::Inscription(info) = entry else {
                        continue;
                    };
                    if !self.lineage.is_ours(&info.this_msg)
                        || self.lineage.intent_live(&info.this_msg)
                    {
                        continue;
                    }
                    match sequencer.handle().publish(info.payload.clone()).await {
                        Ok((result, _checkpoint)) => {
                            self.lineage
                                .record_republish(info.this_msg, result.tx.inscription().this_msg);
                        }
                        Err(error) => warn!(%error, "Failed to re-publish orphaned zone payload"),
                    }
                }
            }
            _ => {}
        }
    }
}

/// Inline policy: republish orphans only when the local balance view still
/// allows it; publish planned payloads as soon as it's our turn to write.
///
/// The balance view follows each update: an extension's `adopted` is applied
/// on top, a conflict rebuilds the non-finalized deltas from
/// `common_prefix ++ adopted` over the finalized ones. A payload we published
/// since the last event is in the prefix as pending, so it is never applied
/// twice.
struct BalanceAwarePolicy {
    balances: BalanceAwareState,
    planned: VecDeque<Inscription>,
    view_rx: tokio::sync::watch::Receiver<SequencerChannelView>,
}

impl<Node> runner::Policy<Node> for BalanceAwarePolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        if let Event::BlocksProcessed {
            channel_update,
            finalized,
            ..
        } = event
        {
            self.balances.record_finalized_payloads(finalized);
            self.balances.observe(channel_update);
            let orphaned_inscriptions: Vec<InscriptionInfo> = channel_update
                .orphaned()
                .iter()
                .filter_map(|o| match o {
                    ChannelUpdateTx::Inscription(i) => Some(i.clone()),
                    ChannelUpdateTx::AtomicWithdraw(_)
                    | ChannelUpdateTx::PinDeposit(_)
                    | ChannelUpdateTx::Custom(_)
                    | ChannelUpdateTx::Config(_) => None,
                })
                .collect();
            for info in orphaned_inscriptions {
                if !self.balances.should_republish(&info.payload) {
                    continue;
                }
                if let Err(error) = sequencer.handle().publish(info.payload.clone()).await {
                    warn!(%error, "Failed to re-publish balance-aware zone payload");
                    continue;
                }
                self.balances.record_republished_payload(&info.payload);
            }
        }

        if !self.view_rx.borrow().our_turn_to_write {
            return;
        }
        while let Some(payload) = self.planned.pop_front() {
            if !self.balances.should_republish(&payload) {
                continue;
            }
            if let Err(error) = sequencer.handle().publish(payload.clone()).await {
                warn!(%error, "Failed to publish planned balance-aware zone payload");
                self.planned.push_front(payload);
                break;
            }
            self.balances.record_republished_payload(&payload);
        }
    }
}

/// Inline policy: republish orphans only when they preserve sorted-payload
/// order; otherwise mark them as discarded.
///
/// The on-chain payload set follows each update: an extension's `adopted`
/// joins it, a conflict rebuilds it from `common_prefix ++ adopted`, so the
/// order floor we gate republishing on falls back correctly when the highest
/// payload is orphaned.
struct SortedConflictPolicy {
    state: SortedConflictState,
}

impl<Node> runner::Policy<Node> for SortedConflictPolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let Event::BlocksProcessed {
            channel_update,
            finalized,
            ..
        } = event
        else {
            return;
        };
        self.state.record_finalized(finalized);
        self.state.observe(channel_update);
        let (orphaned, adopted) = (channel_update.orphaned(), channel_update.adopted());
        let orphaned_inscriptions: Vec<&InscriptionInfo> = orphaned
            .iter()
            .filter_map(|o| match o {
                ChannelUpdateTx::Inscription(i) => Some(i),
                ChannelUpdateTx::AtomicWithdraw(_)
                | ChannelUpdateTx::PinDeposit(_)
                | ChannelUpdateTx::Custom(_)
                | ChannelUpdateTx::Config(_) => None,
            })
            .collect();

        self.state.record_adoptions(adopted).await;

        let readopted: HashSet<&Inscription> = adopted
            .iter()
            .filter_map(|tx| tx.inscription().map(|i| &i.payload))
            .collect();

        // Consider this round's fresh orphans together with everything parked,
        // in sorted order (a `BTreeSet` iterates ascending). A payload parked
        // under a higher floor on another branch then slots in ahead of a higher
        // fresh orphan instead of being locked out, and the chain stays sorted.
        // Finalized payloads are excluded — they're already permanently landed.
        let mut candidates: BTreeSet<Inscription> = orphaned_inscriptions
            .iter()
            .map(|i| i.payload.clone())
            .filter(|payload| !readopted.contains(payload) && !self.state.is_finalized(payload))
            .collect();
        candidates.extend(self.state.discarded_snapshot().await);

        for payload in candidates {
            if self.state.is_finalized(&payload) {
                continue;
            }
            if self.state.preserves_order(&payload) {
                if let Err(error) = sequencer.handle().publish(payload.clone()).await {
                    warn!(%error, "Failed to re-publish sorted zone payload");
                    continue;
                }
                self.state.record_published_payload(payload).await;
            } else {
                self.state.discard(payload).await;
            }
        }
    }
}

struct BalanceAwareState {
    initial_balances: ZoneAccountBalances,
    applied: HashMap<String, HashMap<String, i64>>,
    finalized: HashSet<String>,
}

impl BalanceAwareState {
    fn new(initial_balances: ZoneAccountBalances) -> Self {
        Self {
            initial_balances,
            applied: HashMap::new(),
            finalized: HashSet::new(),
        }
    }

    /// Pin finalized payloads.
    fn record_finalized_payloads(&mut self, finalized: &[FinalizedTx]) {
        for inscription in finalized_inscriptions(finalized) {
            if let Some((uuid, _, _)) = parse_balance_payload(&inscription.payload) {
                self.finalized.insert(uuid);
            }
            self.record_applied_payload(&inscription.payload);
        }
    }

    fn record_applied_payload(&mut self, payload: &Inscription) {
        let Some((uuid, account, delta)) = parse_balance_payload(payload) else {
            return;
        };

        self.applied.entry(account).or_default().insert(uuid, delta);
    }

    /// Apply an extension's `adopted`; on a conflict replace the
    /// non-finalized deltas with the view's. Finalized deltas stay.
    fn observe(&mut self, channel_update: &ChannelUpdate) {
        if let ChannelUpdate::Conflict { .. } = channel_update {
            let finalized = &self.finalized;
            for updates in self.applied.values_mut() {
                updates.retain(|uuid, _| finalized.contains(uuid));
            }
        }
        for info in contributed(channel_update).inscriptions() {
            self.record_applied_payload(&info.payload);
        }
    }

    fn should_republish(&self, payload: &Inscription) -> bool {
        let Some((uuid, account, delta)) = parse_balance_payload(payload) else {
            return false;
        };

        if self.finalized.contains(&uuid) || self.account_updates(&account).contains_key(&uuid) {
            return false;
        }

        self.available_balance(&account) + delta >= 0
    }

    fn record_republished_payload(&mut self, payload: &Inscription) {
        self.record_applied_payload(payload);
    }

    fn available_balance(&self, account: &str) -> i64 {
        self.initial_balances.get(account).copied().unwrap_or(0)
            + self.account_updates(account).values().sum::<i64>()
    }

    fn account_updates(&self, account: &str) -> &HashMap<String, i64> {
        self.applied.get(account).unwrap_or(&EMPTY_BALANCE_UPDATES)
    }
}

static EMPTY_BALANCE_UPDATES: LazyLock<HashMap<String, i64>> = LazyLock::new(HashMap::new);

struct SortedConflictState {
    /// The ordering floor: the non-finalized view as of the last update plus
    /// what we published since, over the pinned finalized base.
    channel_view: BTreeSet<Inscription>,
    discarded: DiscardedPayloads,
    finalized: HashSet<Inscription>,
}

impl SortedConflictState {
    fn new(discarded: DiscardedPayloads) -> Self {
        Self {
            channel_view: BTreeSet::new(),
            discarded,
            finalized: HashSet::new(),
        }
    }

    /// Pin finalized payloads into the channel view permanently.
    fn record_finalized(&mut self, finalized: &[FinalizedTx]) {
        for inscription in finalized_inscriptions(finalized) {
            self.finalized.insert(inscription.payload.clone());
            self.channel_view.insert(inscription.payload.clone());
        }
    }

    fn is_finalized(&self, payload: &Inscription) -> bool {
        self.finalized.contains(payload)
    }

    /// Extend the floor with an extension's `adopted`; on a conflict rebuild
    /// it from the view over the finalized base.
    fn observe(&mut self, channel_update: &ChannelUpdate) {
        if let ChannelUpdate::Conflict { .. } = channel_update {
            self.channel_view = self.finalized.iter().cloned().collect();
        }
        self.channel_view.extend(
            contributed(channel_update)
                .inscriptions()
                .map(|info| info.payload.clone()),
        );
    }

    /// A discarded payload that landed anyway is no longer ours to re-home.
    async fn record_adoptions(&self, adopted: &[ChannelUpdateTx]) {
        for info in adopted.iter().filter_map(ChannelUpdateTx::inscription) {
            self.discarded.lock().await.remove(&info.payload);
        }
    }

    async fn record_published_payload(&mut self, payload: Inscription) {
        self.discarded.lock().await.remove(&payload);
        self.channel_view.insert(payload);
    }

    fn preserves_order(&self, payload: &Inscription) -> bool {
        self.channel_view.last().is_none_or(|max| payload >= max)
    }

    async fn discard(&self, payload: Inscription) {
        self.discarded.lock().await.insert(payload);
    }

    async fn discarded_snapshot(&self) -> Vec<Inscription> {
        self.discarded.lock().await.iter().cloned().collect()
    }
}
