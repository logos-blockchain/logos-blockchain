//! Reaction policies run inline on each sequencer event, ported from the
//! cucumber suite so the simulated scenarios exercise the same consumer logic.

use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use lb_core::{
    crypto::Hash,
    mantle::{
        Note, NoteId,
        ledger::Outputs,
        ops::channel::{ChannelId, MsgId, inscribe::Inscription},
    },
    proofs::channel_multi_sig_proof::IndexedSignatures,
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_zone_sdk::sequencer::{
    ChannelUpdate, ChannelUpdateTx, Error, Event, FinalizedOp, FinalizedTx, InscriptionId,
    InscriptionInfo, PreparedAtomicBundle, SequencerChannelView, WithdrawArg, WithdrawInputs,
    ZoneSequencer, channel_inscriptions,
};
use tokio::sync::{broadcast, watch};
use tracing::warn;

use crate::{node::SimNode, txs};

/// A policy reacting to one sequencer's events from inside its drive loop.
#[async_trait]
pub trait Policy: Send {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event);
}

#[must_use]
pub fn make_inscription(msg: &str) -> Inscription {
    Inscription::try_from(msg.as_bytes().to_vec()).expect("payload fits an inscription")
}

#[must_use]
pub fn payload_str(payload: &Inscription) -> String {
    String::from_utf8_lossy(payload.as_slice()).into_owned()
}

/// Inscriptions in the just-finalized txs.
pub fn finalized_inscriptions(finalized: &[FinalizedTx]) -> impl Iterator<Item = &InscriptionInfo> {
    finalized
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .filter_map(|op| match op {
            FinalizedOp::Inscription(info) => Some(info),
            FinalizedOp::Deposit(_)
            | FinalizedOp::Withdraw(_)
            | FinalizedOp::Config(_)
            | FinalizedOp::ChannelTransfer(_) => None,
        })
}

/// The inscriptions carried by channel-update entries, in order.
pub trait Inscriptions<'a> {
    fn inscriptions(self) -> impl Iterator<Item = &'a InscriptionInfo>;
}

impl<'a, I: IntoIterator<Item = &'a ChannelUpdateTx>> Inscriptions<'a> for I {
    fn inscriptions(self) -> impl Iterator<Item = &'a InscriptionInfo> {
        self.into_iter().filter_map(ChannelUpdateTx::inscription)
    }
}

/// The entries an update contributes to a consumer's non-finalized view: a
/// conflict's whole `canonical_chain()`, an extension's `adopted`.
pub fn contributed(update: &ChannelUpdate) -> impl Iterator<Item = &ChannelUpdateTx> {
    let prefix = match update {
        ChannelUpdate::Conflict { common_prefix, .. } => common_prefix.as_slice(),
        ChannelUpdate::Extension { .. } => &[],
    };
    prefix.iter().chain(update.adopted())
}

#[must_use]
pub fn balance_update_payload(uuid: &str, account: &str, delta: i64) -> Inscription {
    make_inscription(&format!("{uuid}:{account}:{delta}"))
}

#[must_use]
pub fn parse_balance_payload(payload: &Inscription) -> Option<(String, String, i64)> {
    let payload = std::str::from_utf8(payload.as_slice()).ok()?;
    let parts = payload.splitn(3, ':').collect::<Vec<_>>();
    let [uuid, account, delta] = parts.as_slice() else {
        return None;
    };
    Some((
        (*uuid).to_owned(),
        (*account).to_owned(),
        delta.parse().ok()?,
    ))
}

#[must_use]
pub fn pin_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("pin deposit {op_id:?}"))
}

#[must_use]
pub fn withdraw_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("withdraw deposit {op_id:?}"))
}

/// Does nothing; the test drives the sequencer directly.
pub struct PassivePolicy;

#[async_trait]
impl Policy for PassivePolicy {
    async fn on_event(&mut self, _sequencer: &mut ZoneSequencer<SimNode>, _event: &Event) {}
}

/// Republish orphaned inscriptions not already back on the canonical chain.
#[derive(Default)]
pub struct OrphanRepublishPolicy {
    finalized: HashSet<Inscription>,
}

#[async_trait]
impl Policy for OrphanRepublishPolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
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

#[derive(Default)]
struct LineageTracker {
    intent_root: HashMap<MsgId, MsgId>,
    pending: HashMap<MsgId, HashSet<MsgId>>,
    finalized_roots: HashSet<MsgId>,
}

impl LineageTracker {
    fn record_publish(&mut self, this_msg: MsgId) {
        self.intent_root.insert(this_msg, this_msg);
        self.pending.entry(this_msg).or_default().insert(this_msg);
    }

    fn record_republish(&mut self, orphan: MsgId, republished: MsgId) {
        let root = self.intent_root.get(&orphan).copied().unwrap_or(orphan);
        self.intent_root.insert(republished, root);
        self.pending.entry(root).or_default().insert(republished);
    }

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

    fn observe_finalized(&mut self, finalized: impl Iterator<Item = MsgId>) {
        for this_msg in finalized {
            if let Some(&root) = self.intent_root.get(&this_msg) {
                self.finalized_roots.insert(root);
            }
        }
    }

    fn is_ours(&self, this_msg: &MsgId) -> bool {
        self.intent_root.contains_key(this_msg)
    }

    fn intent_live(&self, this_msg: &MsgId) -> bool {
        let root = self.intent_root.get(this_msg).copied().unwrap_or(*this_msg);
        self.finalized_roots.contains(&root)
            || self
                .pending
                .get(&root)
                .is_some_and(|members| !members.is_empty())
    }
}

/// Publish `planned` once ready, then republish our own orphans whose intent
/// has no live member, tracking msg-id lineage so repeated payloads work.
pub struct RepublishLineagePolicy {
    planned: Vec<Inscription>,
    published_initial: bool,
    lineage: LineageTracker,
}

impl RepublishLineagePolicy {
    #[must_use]
    pub fn new(planned: Vec<Inscription>) -> Self {
        Self {
            planned,
            published_initial: false,
            lineage: LineageTracker::default(),
        }
    }
}

#[async_trait]
impl Policy for RepublishLineagePolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
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

pub type ZoneAccountBalances = HashMap<String, i64>;

struct BalanceAwareState {
    initial_balances: ZoneAccountBalances,
    applied: HashMap<String, HashMap<String, i64>>,
    finalized: HashSet<String>,
}

impl BalanceAwareState {
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

    fn available_balance(&self, account: &str) -> i64 {
        self.initial_balances.get(account).copied().unwrap_or(0)
            + self.account_updates(account).values().sum::<i64>()
    }

    fn account_updates(&self, account: &str) -> HashMap<String, i64> {
        self.applied.get(account).cloned().unwrap_or_default()
    }
}

/// Republish orphans only when the local balance view still allows it;
/// publish planned payloads as soon as it is our turn to write.
pub struct BalanceAwarePolicy {
    balances: BalanceAwareState,
    planned: VecDeque<Inscription>,
    view_rx: watch::Receiver<SequencerChannelView>,
}

impl BalanceAwarePolicy {
    #[must_use]
    pub fn new(
        sequencer: &ZoneSequencer<SimNode>,
        initial_balances: ZoneAccountBalances,
        planned: Vec<Inscription>,
    ) -> Self {
        Self {
            balances: BalanceAwareState {
                initial_balances,
                applied: HashMap::new(),
                finalized: HashSet::new(),
            },
            planned: VecDeque::from(planned),
            view_rx: sequencer.subscribe_channel_view(),
        }
    }
}

#[async_trait]
impl Policy for BalanceAwarePolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
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
                    _ => None,
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
                self.balances.record_applied_payload(&info.payload);
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
            self.balances.record_applied_payload(&payload);
        }
    }
}

/// Payloads the sorted-conflict policies gave up on, shared by all sequencers
/// of a scenario.
pub type DiscardedPayloads = Arc<Mutex<HashSet<Inscription>>>;

/// Republish orphans only when they preserve sorted-payload order; otherwise
/// mark them as discarded.
pub struct SortedConflictPolicy {
    channel_view: BTreeSet<Inscription>,
    discarded: DiscardedPayloads,
    finalized: HashSet<Inscription>,
}

impl SortedConflictPolicy {
    #[must_use]
    pub fn new(discarded: &DiscardedPayloads) -> Self {
        Self {
            channel_view: BTreeSet::new(),
            discarded: Arc::clone(discarded),
            finalized: HashSet::new(),
        }
    }

    fn preserves_order(&self, payload: &Inscription) -> bool {
        self.channel_view.last().is_none_or(|max| payload >= max)
    }
}

#[async_trait]
impl Policy for SortedConflictPolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
        let Event::BlocksProcessed {
            channel_update,
            finalized,
            ..
        } = event
        else {
            return;
        };
        for inscription in finalized_inscriptions(finalized) {
            self.finalized.insert(inscription.payload.clone());
            self.channel_view.insert(inscription.payload.clone());
        }
        if let ChannelUpdate::Conflict { .. } = channel_update {
            self.channel_view = self.finalized.iter().cloned().collect();
        }
        self.channel_view.extend(
            contributed(channel_update)
                .inscriptions()
                .map(|info| info.payload.clone()),
        );
        let adopted = channel_update.adopted();
        for info in adopted.iter().filter_map(ChannelUpdateTx::inscription) {
            self.discarded.lock().unwrap().remove(&info.payload);
        }
        let readopted: HashSet<&Inscription> = adopted
            .iter()
            .filter_map(|tx| tx.inscription().map(|i| &i.payload))
            .collect();
        let mut candidates: BTreeSet<Inscription> = channel_update
            .orphaned()
            .inscriptions()
            .map(|i| i.payload.clone())
            .filter(|payload| !readopted.contains(payload) && !self.finalized.contains(payload))
            .collect();
        candidates.extend(self.discarded.lock().unwrap().iter().cloned());

        for payload in candidates {
            if self.finalized.contains(&payload) {
                continue;
            }
            if self.preserves_order(&payload) {
                if let Err(error) = sequencer.handle().publish(payload.clone()).await {
                    warn!(%error, "Failed to re-publish sorted zone payload");
                    continue;
                }
                self.discarded.lock().unwrap().remove(&payload);
                self.channel_view.insert(payload);
            } else {
                self.discarded.lock().unwrap().insert(payload);
            }
        }
    }
}

struct DepositLifecycleState {
    notes: Vec<NoteId>,
    pinned: bool,
    withdrawn: bool,
}

fn wallet_notes(sequencer: &ZoneSequencer<SimNode>) -> HashSet<NoteId> {
    let view = sequencer.channel_wallet();
    view.finalized
        .iter()
        .chain(view.unfinalized.iter())
        .map(|note| note.note_id)
        .collect()
}

fn withdraw_outputs(values: &[u64], recipient: ZkPublicKey) -> Option<Outputs> {
    Outputs::try_new(
        values
            .iter()
            .map(|&v| Note::new(v, recipient))
            .collect::<Vec<_>>(),
    )
    .ok()
}

/// Reconcile deposits against branch state and the view, then decide per
/// deposit whether to pin or withdraw.
fn reconcile_deposits(
    deposits: &mut HashMap<Hash, DepositLifecycleState>,
    finalized: &mut HashSet<Inscription>,
    event: &Event,
) -> bool {
    let Event::BlocksProcessed {
        channel_update,
        deposits: observed,
        finalized: finalized_txs,
        ..
    } = event
    else {
        return false;
    };
    for deposit in observed {
        deposits
            .entry(deposit.op_id)
            .or_insert_with(|| DepositLifecycleState {
                notes: deposit.notes.iter().map(|note| note.note_id).collect(),
                pinned: false,
                withdrawn: false,
            });
    }
    finalized.extend(finalized_inscriptions(finalized_txs).map(|info| info.payload.clone()));
    let rebuild = matches!(channel_update, ChannelUpdate::Conflict { .. });
    let payloads: HashSet<&Inscription> = contributed(channel_update)
        .inscriptions()
        .map(|i| &i.payload)
        .collect();
    for (op_id, state) in deposits.iter_mut() {
        let present = |payload: &Inscription, was: bool| {
            finalized.contains(payload) || payloads.contains(payload) || (!rebuild && was)
        };
        state.pinned = present(&pin_payload(op_id), state.pinned);
        state.withdrawn = present(&withdraw_payload(op_id), state.withdrawn);
    }
    true
}

/// Pin each observed deposit, then withdraw the re-created note, without
/// waiting for finalization.
pub struct DepositLifecyclePolicy {
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    deposits: HashMap<Hash, DepositLifecycleState>,
    finalized: HashSet<Inscription>,
}

impl DepositLifecyclePolicy {
    #[must_use]
    pub fn new(withdraw_outputs: Vec<u64>, recipient: ZkPublicKey) -> Self {
        Self {
            withdraw_outputs,
            recipient,
            deposits: HashMap::new(),
            finalized: HashSet::new(),
        }
    }
}

#[async_trait]
impl Policy for DepositLifecyclePolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
        if !reconcile_deposits(&mut self.deposits, &mut self.finalized, event) {
            return;
        }
        let wallet = wallet_notes(sequencer);
        for (op_id, state) in &mut self.deposits {
            if state.notes.is_empty() {
                continue;
            }
            let on_branch = state.notes.iter().all(|id| wallet.contains(id));
            if !state.pinned && on_branch {
                match sequencer
                    .handle()
                    .publish_pin_deposit(pin_payload(op_id), state.notes.clone())
                    .await
                {
                    Ok(_) => state.pinned = true,
                    Err(error) => warn!(%error, "deposit-pin inscription failed"),
                }
            } else if !state.withdrawn && state.pinned && !on_branch {
                let Some(outputs) = withdraw_outputs(&self.withdraw_outputs, self.recipient) else {
                    continue;
                };
                match sequencer
                    .handle()
                    .publish_atomic_withdraw(
                        withdraw_payload(op_id),
                        vec![WithdrawArg { outputs }],
                        WithdrawInputs::Auto,
                    )
                    .await
                {
                    Ok(_) => state.withdrawn = true,
                    Err(error) => warn!(%error, "deposit-withdraw failed"),
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MultiSigPhase {
    Pin,
    Withdraw,
}

type BundleId = Vec<u8>;

pub struct MultiSigRound {
    prepared: Arc<PreparedAtomicBundle>,
    signatures: IndexedSignatures,
    op_id: Hash,
}

/// Signatures gathered per bundle; the proposer reads it back to submit.
pub type MultiSigBus = Arc<Mutex<HashMap<BundleId, MultiSigRound>>>;

/// Fanout channel proposers announce bundles on and signers read.
pub type BundleAnnounce = broadcast::Sender<Arc<PreparedAtomicBundle>>;

/// Multi-sig counterpart of [`DepositLifecyclePolicy`]: each sequencer
/// proposes its own bundles, everyone signs every bundle, each submits its
/// own. Announcements are drained before each event is handled.
pub struct MultiSigLifecyclePolicy {
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    bus: MultiSigBus,
    announce: BundleAnnounce,
    announcements: broadcast::Receiver<Arc<PreparedAtomicBundle>>,
    signing_key: Ed25519Key,
    signed: HashSet<BundleId>,
    deposits: HashMap<Hash, DepositLifecycleState>,
    finalized: HashSet<Inscription>,
    mine: HashMap<(Hash, MultiSigPhase), BundleId>,
}

impl MultiSigLifecyclePolicy {
    #[must_use]
    pub fn new(
        withdraw_outputs: Vec<u64>,
        recipient: ZkPublicKey,
        bus: MultiSigBus,
        announce: BundleAnnounce,
        signing_key: Ed25519Key,
    ) -> Self {
        let announcements = announce.subscribe();
        Self {
            withdraw_outputs,
            recipient,
            bus,
            announce,
            announcements,
            signing_key,
            signed: HashSet::new(),
            deposits: HashMap::new(),
            finalized: HashSet::new(),
            mine: HashMap::new(),
        }
    }

    /// Sign every announced bundle we have not signed yet.
    pub fn drain_announcements(&mut self) {
        loop {
            match self.announcements.try_recv() {
                Ok(bundle) => self.sign_announced(&bundle),
                Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    }

    fn sign_announced(&mut self, prepared: &PreparedAtomicBundle) {
        let id = prepared.sign_payload.clone();
        if !self.signed.insert(id.clone()) {
            return;
        }
        match prepared.sign_with(&self.signing_key) {
            Ok(signature) => {
                if let Some(round) = self.bus.lock().unwrap().get_mut(&id)
                    && let Err(error) = round
                        .signatures
                        .try_insert(signature.channel_key_index, signature.signature)
                {
                    warn!(?error, "multi-sig signer: signature not recorded");
                }
            }
            Err(error) => warn!(%error, "multi-sig signer: cannot sign bundle"),
        }
    }

    fn open_round(&mut self, work: (Hash, MultiSigPhase), prepared: PreparedAtomicBundle) {
        let id = prepared.sign_payload.clone();
        let prepared = Arc::new(prepared);
        self.bus.lock().unwrap().insert(
            id.clone(),
            MultiSigRound {
                prepared: Arc::clone(&prepared),
                signatures: IndexedSignatures::empty(),
                op_id: work.0,
            },
        );
        self.mine.insert(work, id);
        drop(self.announce.send(prepared));
    }

    fn close_round(&mut self, work: &(Hash, MultiSigPhase)) {
        if let Some(id) = self.mine.remove(work) {
            self.bus.lock().unwrap().remove(&id);
        }
    }

    fn submit_ready(
        &self,
        sequencer: &mut ZoneSequencer<SimNode>,
        id: &BundleId,
    ) -> Result<bool, Error> {
        let ready = {
            let bus = self.bus.lock().unwrap();
            bus.get(id)
                .filter(|round| {
                    round.signatures.len() >= usize::from(round.prepared.signing_threshold)
                })
                .map(|round| {
                    let threshold = usize::from(round.prepared.signing_threshold);
                    let signatures = IndexedSignatures::try_from_iter(
                        round
                            .signatures
                            .iter()
                            .take(threshold)
                            .map(|(index, signature)| (*index, *signature)),
                    )
                    .expect("a subset of a bounded map fits");
                    (round.prepared.as_ref().clone(), signatures)
                })
        };
        let Some((prepared, signatures)) = ready else {
            return Ok(false);
        };
        sequencer
            .handle()
            .submit_atomic_bundle(prepared, signatures)
            .map(|_| true)
    }
}

#[async_trait]
impl Policy for MultiSigLifecyclePolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
        self.drain_announcements();
        if !reconcile_deposits(&mut self.deposits, &mut self.finalized, event) {
            return;
        }
        let wallet = wallet_notes(sequencer);
        let work_items: Vec<(Hash, MultiSigPhase, Vec<NoteId>)> = self
            .deposits
            .iter()
            .filter(|(_, state)| !state.notes.is_empty())
            .filter_map(|(op_id, state)| {
                let on_branch = state.notes.iter().all(|id| wallet.contains(id));
                let phase = if !state.pinned && on_branch {
                    MultiSigPhase::Pin
                } else if !state.withdrawn && state.pinned && !on_branch {
                    MultiSigPhase::Withdraw
                } else {
                    return None;
                };
                Some((*op_id, phase, state.notes.clone()))
            })
            .collect();
        for (op_id, phase, notes) in work_items {
            let work = (op_id, phase);
            let Some(id) = self.mine.get(&work).cloned() else {
                let prepared = match phase {
                    MultiSigPhase::Pin => sequencer
                        .handle()
                        .prepare_pin_deposit(pin_payload(&op_id), notes)
                        .await
                        .map_err(|error| warn!(%error, "multi-sig pin prepare failed"))
                        .ok(),
                    MultiSigPhase::Withdraw => {
                        match withdraw_outputs(&self.withdraw_outputs, self.recipient) {
                            Some(outputs) => sequencer
                                .handle()
                                .prepare_atomic_withdraw(
                                    withdraw_payload(&op_id),
                                    vec![WithdrawArg { outputs }],
                                    WithdrawInputs::Auto,
                                )
                                .await
                                .map_err(|error| warn!(%error, "multi-sig withdraw prepare failed"))
                                .ok(),
                            None => None,
                        }
                    }
                };
                if let Some(prepared) = prepared {
                    self.open_round(work, prepared);
                }
                continue;
            };
            match self.submit_ready(sequencer, &id) {
                Ok(true) => {
                    let state = self.deposits.get_mut(&op_id).expect("tracked");
                    match phase {
                        MultiSigPhase::Pin => state.pinned = true,
                        MultiSigPhase::Withdraw => state.withdrawn = true,
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, "multi-sig bundle submit failed");
                    self.close_round(&work);
                }
            }
        }
        let withdrawn: HashSet<Hash> = self
            .deposits
            .iter()
            .filter(|(_, state)| state.withdrawn)
            .map(|(op_id, _)| *op_id)
            .collect();
        if !withdrawn.is_empty() {
            self.bus
                .lock()
                .unwrap()
                .retain(|_, round| !withdrawn.contains(&round.op_id));
            self.mine.retain(|(op_id, _), _| !withdrawn.contains(op_id));
        }
    }
}

struct DepositWithdrawState {
    notes: Vec<NoteId>,
    withdraw_tx: Option<InscriptionId>,
}

/// Withdraw the deposit of `target_amount` reactively, `Auto` sweeping other
/// notes, with no pin step.
pub struct DepositWithdrawPolicy {
    target_amount: u64,
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    deposits: HashMap<Hash, DepositWithdrawState>,
}

impl DepositWithdrawPolicy {
    #[must_use]
    pub fn new(target_amount: u64, withdraw_outputs: Vec<u64>, recipient: ZkPublicKey) -> Self {
        Self {
            target_amount,
            withdraw_outputs,
            recipient,
            deposits: HashMap::new(),
        }
    }
}

#[async_trait]
impl Policy for DepositWithdrawPolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
        let Event::BlocksProcessed {
            channel_update,
            deposits: observed,
            finalized,
            ..
        } = event
        else {
            return;
        };
        for deposit in observed {
            if deposit.amount == self.target_amount {
                self.deposits
                    .entry(deposit.op_id)
                    .or_insert_with(|| DepositWithdrawState {
                        notes: deposit.notes.iter().map(|note| note.note_id).collect(),
                        withdraw_tx: None,
                    });
            }
        }
        if let Some(chain) = channel_update.canonical_chain() {
            let live: HashSet<InscriptionId> = chain
                .map(ChannelUpdateTx::tx_hash)
                .chain(finalized.iter().map(|tx| tx.tx_hash))
                .collect();
            for state in self.deposits.values_mut() {
                state.withdraw_tx = state.withdraw_tx.filter(|hash| live.contains(hash));
            }
        }
        let wallet = wallet_notes(sequencer);
        for (op_id, state) in &mut self.deposits {
            let on_branch =
                !state.notes.is_empty() && state.notes.iter().all(|id| wallet.contains(id));
            if state.withdraw_tx.is_none() && on_branch {
                let Some(outputs) = withdraw_outputs(&self.withdraw_outputs, self.recipient) else {
                    continue;
                };
                match sequencer
                    .handle()
                    .publish_atomic_withdraw(
                        withdraw_payload(op_id),
                        vec![WithdrawArg { outputs }],
                        WithdrawInputs::Auto,
                    )
                    .await
                {
                    Ok((result, _)) => state.withdraw_tx = Some(result.inscription_id()),
                    Err(error) => warn!(%error, "deposit-withdraw failed"),
                }
            }
        }
    }
}

/// Orphan-republish for hand-built multi-inscription txs: orphans that are
/// neither in the view nor finalized are rebuilt and re-submitted.
pub struct CustomRepublishPolicy {
    node: SimNode,
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    funding_pk: ZkPublicKey,
    batches: VecDeque<Vec<Inscription>>,
    view_rx: watch::Receiver<SequencerChannelView>,
    finalized: HashSet<Inscription>,
    chain_tip: Option<MsgId>,
    ready: bool,
}

impl CustomRepublishPolicy {
    #[must_use]
    pub fn new(
        sequencer: &ZoneSequencer<SimNode>,
        node: SimNode,
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        funding_pk: ZkPublicKey,
        batches: Vec<Vec<Inscription>>,
    ) -> Self {
        Self {
            node,
            channel_id,
            signing_key,
            funding_pk,
            batches: batches.into(),
            view_rx: sequencer.subscribe_channel_view(),
            finalized: HashSet::new(),
            chain_tip: None,
            ready: false,
        }
    }

    async fn submit(
        &mut self,
        sequencer: &mut ZoneSequencer<SimNode>,
        payloads: Vec<Inscription>,
    ) -> bool {
        let parent = self
            .chain_tip
            .unwrap_or_else(|| self.view_rx.borrow().tip_message);
        let built = txs::build_funded_custom_tx(
            &self.node,
            self.channel_id,
            &self.signing_key,
            self.funding_pk,
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
            Ok(_) => {
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
            ChannelUpdateTx::Custom(tx) => channel_inscriptions(tx, self.channel_id)
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

#[async_trait]
impl Policy for CustomRepublishPolicy {
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<SimNode>, event: &Event) {
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
            Event::TurnNotification { .. } => return,
        };
        if let Some(finalized) = finalized {
            self.finalized
                .extend(finalized_inscriptions(finalized).map(|info| info.payload.clone()));
        }
        if let Some(channel_update) = channel_update {
            let orphaned: HashSet<Inscription> = channel_update
                .orphaned()
                .iter()
                .flat_map(|entry| self.entry_payloads(entry))
                .collect();
            let view: HashSet<Inscription> = channel_update
                .canonical_chain()
                .into_iter()
                .flatten()
                .flat_map(|entry| self.entry_payloads(entry))
                .collect();
            let republish: Vec<Inscription> = orphaned
                .into_iter()
                .filter(|payload| !view.contains(payload) && !self.finalized.contains(payload))
                .collect();
            if self.ready && !republish.is_empty() {
                self.chain_tip = None;
                if !self.submit(sequencer, republish.clone()).await {
                    self.batches.push_back(republish);
                }
            }
        }
        while self.ready {
            let Some(batch) = self.batches.pop_front() else {
                break;
            };
            if !self.submit(sequencer, batch.clone()).await {
                self.batches.push_front(batch);
                break;
            }
        }
    }
}
