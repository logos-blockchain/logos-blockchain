use super::{
    Arc, BTreeSet, ChannelUpdate, ChannelUpdateTx, DiscardedPayloads, Event, FinalizedTx, HashMap,
    HashSet, Inscription, InscriptionInfo, LazyLock, MsgId, PolicyRuntime, SequencerChannelView,
    VecDeque, ZoneAccountBalances, ZoneNodeHttpClient, ZoneSequencer, finalized_inscriptions,
    parse_balance_payload, runner, to_policy_runtime, warn,
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
/// Tracks canonical on-chain state keyed by id, decided by payload (see
/// `on_event`): a dead twin's id leaves while a live twin keeps the payload
/// covered, so a payload still on chain is never re-homed.
#[derive(Default)]
struct OrphanRepublishPolicy {
    /// Canonical on-chain inscriptions by id (adopted-unfinalized + finalized).
    /// Finalized entries are added and never removed — they can't be orphaned —
    /// so this one set is the whole on-chain view.
    canonical: HashMap<MsgId, Inscription>,
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
        // 1. Remove orphaned by id — a dead twin leaves, a live twin stays.
        for entry in &channel_update.orphaned {
            if let Some(info) = entry.inscription() {
                self.canonical.remove(&info.this_msg);
            }
        }
        // 2. Add finalized by id (permanent — finalized can't be orphaned).
        for info in finalized_inscriptions(finalized) {
            self.canonical.insert(info.this_msg, info.payload.clone());
        }
        // 3. Add adopted by id (this block's new canonical).
        for info in channel_update
            .adopted
            .iter()
            .filter_map(ChannelUpdateTx::inscription)
        {
            self.canonical.insert(info.this_msg, info.payload.clone());
        }
        // 4. Republish orphaned whose payload no canonical id still carries.
        for entry in &channel_update.orphaned {
            let ChannelUpdateTx::Inscription(info) = entry else {
                continue;
            };
            if self
                .canonical
                .values()
                .any(|payload| *payload == info.payload)
            {
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
    /// Per intent root, the `this_msg`s currently pending (in the
    /// non-finalized channel view).
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

    /// Fold a delta into per-intent liveness — only our `msg_id`s are relevant.
    /// Adopted members become live; orphaned members stop being live.
    fn observe(&mut self, channel_update: &ChannelUpdate) {
        for info in channel_update
            .adopted
            .iter()
            .filter_map(ChannelUpdateTx::inscription)
        {
            if let Some(&root) = self.intent_root.get(&info.this_msg) {
                self.pending.entry(root).or_default().insert(info.this_msg);
            }
        }
        for entry in &channel_update.orphaned {
            if let ChannelUpdateTx::Inscription(info) = entry
                && let Some(&root) = self.intent_root.get(&info.this_msg)
                && let Some(members) = self.pending.get_mut(&root)
            {
                members.remove(&info.this_msg);
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
                for entry in &channel_update.orphaned {
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
/// The balance view is rebuilt from the full delta — every orphaned op is
/// removed and every adopted op applied — so affordability reflects all
/// inscriptions on the channel. Removing an orphan we never applied (never-
/// landed pending) is a no-op, and an already-adopted op is skipped because its
/// id is already in the applied set after `record_adopted_payloads`.
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
            let ChannelUpdate {
                orphaned, adopted, ..
            } = channel_update;
            let orphaned_inscriptions: Vec<InscriptionInfo> = orphaned
                .iter()
                .filter_map(|o| match o {
                    ChannelUpdateTx::Inscription(i) => Some(i.clone()),
                    ChannelUpdateTx::AtomicWithdraw(_)
                    | ChannelUpdateTx::PinDeposit(_)
                    | ChannelUpdateTx::Custom(_)
                    | ChannelUpdateTx::Config(_) => None,
                })
                .collect();
            self.balances
                .remove_orphaned_payloads(&orphaned_inscriptions);
            self.balances.record_adopted_payloads(adopted);
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
/// The full delta lets us rebuild the on-chain payload set each update (drop
/// orphaned, add adopted), so the order floor we gate republishing on falls
/// back correctly when the highest payload is orphaned.
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
        // Pin finalized payloads first.
        self.state.record_finalized(finalized);
        let ChannelUpdate {
            orphaned, adopted, ..
        } = channel_update;
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

        // Rebuild on-chain state from this delta before deciding anything.
        self.state.revert_orphaned(&orphaned_inscriptions);
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

    fn remove_orphaned_payloads(&mut self, orphaned: &[InscriptionInfo]) {
        for inscription in orphaned {
            let Some((uuid, account, _)) = parse_balance_payload(&inscription.payload) else {
                continue;
            };

            // A finalized delta is permanent — never drop it on an orphan.
            if self.finalized.contains(&uuid) {
                continue;
            }

            if let Some(account_updates) = self.applied.get_mut(&account) {
                account_updates.remove(&uuid);
            }
        }
    }

    fn record_adopted_payloads(&mut self, adopted: &[ChannelUpdateTx]) {
        for info in adopted.iter().filter_map(ChannelUpdateTx::inscription) {
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
    /// The local channel view: pending (non-finalized) payloads plus the
    /// pinned finalized base, kept as the ordering floor.
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

    /// Drop orphaned payloads from the channel view — the order floor falls
    /// back to the max of whatever remains. Finalized payloads stay put.
    fn revert_orphaned(&mut self, orphaned: &[&InscriptionInfo]) {
        for inscription in orphaned {
            if self.finalized.contains(&inscription.payload) {
                continue;
            }
            self.channel_view.remove(&inscription.payload);
        }
    }

    async fn record_adoptions(&mut self, adopted: &[ChannelUpdateTx]) {
        for info in adopted.iter().filter_map(ChannelUpdateTx::inscription) {
            self.discarded.lock().await.remove(&info.payload);
            self.channel_view.insert(info.payload.clone());
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
