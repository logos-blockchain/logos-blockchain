use std::sync::{Arc, Mutex};

use lb_key_management_system_service::keys::Ed25519Key;
use tokio::sync::broadcast;

use super::{
    ChannelUpdate, ChannelUpdateTx, Event, FinalizedTx, Hash, HashMap, HashSet, IndexedSignatures,
    Inscription, InscriptionId, Inscriptions as _, Note, NoteId, Outputs, PolicyRuntime,
    PreparedAtomicBundle, WithdrawArg, WithdrawInputs, ZkPublicKey, ZoneNodeHttpClient,
    ZoneSequencer, contributed, finalized_inscriptions, make_inscription, runner,
    to_policy_runtime, warn,
};

/// Reactively drive the full deposit lifecycle (pin, then withdraw the
/// re-created note), no wait for finalization. See [`DepositLifecyclePolicy`].
pub fn start_deposit_lifecycle_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
) -> PolicyRuntime {
    let policy = DepositLifecyclePolicy {
        withdraw_outputs,
        recipient,
        deposits: HashMap::new(),
        finalized: HashSet::new(),
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

struct DepositLifecycleState {
    notes: Vec<NoteId>,
    pinned: bool,
    withdrawn: bool,
}

/// Reconciles each observed deposit against branch state (fork- and
/// multi-sequencer-correct), matching phases by the deposit's `op_id`:
/// pin it, then withdraw the re-created note.
struct DepositLifecyclePolicy {
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    deposits: HashMap<Hash, DepositLifecycleState>,
    /// Finalized payloads: permanently on chain.
    finalized: HashSet<Inscription>,
}

pub fn pin_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("pin deposit {op_id:?}"))
}

pub fn withdraw_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("withdraw deposit {op_id:?}"))
}

async fn publish_deposit_inscription<Node>(
    sequencer: &mut ZoneSequencer<Node>,
    inscription: Inscription,
    notes: Vec<NoteId>,
) -> Option<InscriptionId>
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    match sequencer
        .handle()
        .publish_pin_deposit(inscription, notes)
        .await
    {
        Ok((result, _)) => Some(result.inscription_id()),
        Err(error) => {
            warn!(%error, "deposit-pin inscription failed");
            None
        }
    }
}

async fn publish_deposit_withdraw<Node>(
    sequencer: &mut ZoneSequencer<Node>,
    inscription: Inscription,
    withdraw_outputs: &[u64],
    recipient: ZkPublicKey,
) -> Option<InscriptionId>
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    let outputs: Vec<Note> = withdraw_outputs
        .iter()
        .map(|&value| Note::new(value, recipient))
        .collect();
    let outputs = Outputs::try_new(outputs).ok()?;
    match sequencer
        .handle()
        .publish_atomic_withdraw(
            inscription,
            vec![WithdrawArg { outputs }],
            WithdrawInputs::Auto,
        )
        .await
    {
        Ok((result, _)) => Some(result.inscription_id()),
        Err(error) => {
            warn!(%error, "deposit-withdraw failed");
            None
        }
    }
}

impl<Node> runner::Policy<Node> for DepositLifecyclePolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let Event::BlocksProcessed {
            channel_update,
            deposits: observed,
            finalized: finalized_txs,
            ..
        } = event
        else {
            return;
        };

        let Self {
            withdraw_outputs,
            recipient,
            deposits,
            finalized,
        } = self;

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
        // An extension only adds steps; a conflict recomputes them from the view.
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

        let wallet: HashSet<NoteId> = {
            let view = sequencer.channel_wallet();
            view.finalized
                .iter()
                .chain(view.unfinalized.iter())
                .map(|note| note.note_id)
                .collect()
        };

        for (op_id, state) in deposits.iter_mut() {
            if state.notes.is_empty() {
                continue;
            }
            let deposit_on_branch = state.notes.iter().all(|id| wallet.contains(id));
            if !state.pinned && deposit_on_branch {
                let inscription = pin_payload(op_id);
                if publish_deposit_inscription(sequencer, inscription, state.notes.clone())
                    .await
                    .is_some()
                {
                    state.pinned = true;
                }
            } else if !state.withdrawn && state.pinned && !deposit_on_branch {
                let inscription = withdraw_payload(op_id);
                if publish_deposit_withdraw(sequencer, inscription, withdraw_outputs, *recipient)
                    .await
                    .is_some()
                {
                    state.withdrawn = true;
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

/// A bundle's identity: its own signing payload. Distinct per proposer, since
/// each bundle carries that proposer's own inscription.
type BundleId = Vec<u8>;

pub struct MultiSigRound {
    prepared: Arc<PreparedAtomicBundle>,
    signatures: IndexedSignatures,
    op_id: Hash,
}

/// Signatures gathered per bundle; the proposer reads it back to submit.
pub type MultiSigBus = Arc<Mutex<HashMap<BundleId, MultiSigRound>>>;

/// Fanout channel proposers announce bundles on and signers read — the test's
/// stand-in for gossip, so signing reacts to announcements, not block events.
pub type BundleAnnounce = broadcast::Sender<Arc<PreparedAtomicBundle>>;

/// Multi-sig counterpart of [`start_deposit_lifecycle_policy`], with no turn
/// logic: each sequencer proposes its own bundles, everyone signs every bundle,
/// each submits its own. The SDK holds a submission until that sequencer's
/// turn, so one lands and note-consumption kills the losers. One task per
/// sequencer, `select!`ing the SDK event stream (chain) against the
/// announcement bus (signing) — the shape a real zone's sequencer has.
pub fn start_multisig_lifecycle_policy(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    bus: MultiSigBus,
    announce: BundleAnnounce,
    signing_key: Ed25519Key,
) -> PolicyRuntime {
    // Subscriptions taken before the sequencer moves into the task (what
    // `runner::spawn` does; bypassed here for the extra `select!` arm).
    let checkpoint_rx = sequencer.subscribe_checkpoint();
    let ready_rx = sequencer.subscribe_ready();
    let channel_view_rx = sequencer.subscribe_channel_view();
    let turn_to_write_rx = sequencer.subscribe_turn_to_write();
    let event_rx = sequencer.subscribe_events();
    let client = sequencer.client();
    let view_violation = runner::ViewViolation::default();

    let mut announcements = announce.subscribe();
    let mut policy = MultiSigLifecyclePolicy {
        withdraw_outputs,
        recipient,
        bus,
        announce,
        signing_key,
        signed: HashSet::new(),
        deposits: HashMap::new(),
        finalized: HashSet::new(),
        mine: HashMap::new(),
    };
    let mut view = runner::ViewChecker::new(Arc::clone(&view_violation));

    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                event = sequencer.next_event() => {
                    view.observe(&event);
                    policy.on_event(&mut sequencer, &event).await;
                }
                announced = announcements.recv() => match announced {
                    Ok(bundle) => policy.sign_announced(&bundle),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    });

    to_policy_runtime(runner::Runtime {
        task,
        view_violation,
        client,
        event_rx,
        checkpoint_rx,
        ready_rx,
        channel_view_rx,
        turn_to_write_rx,
    })
}

struct MultiSigLifecyclePolicy {
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    bus: MultiSigBus,
    announce: BundleAnnounce,
    signing_key: Ed25519Key,
    signed: HashSet<BundleId>,
    deposits: HashMap<Hash, DepositLifecycleState>,
    /// Finalized payloads: permanently on chain.
    finalized: HashSet<Inscription>,
    /// The bundle we ourselves proposed per work item — the one we submit.
    mine: HashMap<(Hash, MultiSigPhase), BundleId>,
}

async fn prepare_pin<Node>(
    sequencer: &mut ZoneSequencer<Node>,
    op_id: &Hash,
    notes: &[NoteId],
) -> Option<PreparedAtomicBundle>
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    match sequencer
        .handle()
        .prepare_pin_deposit(pin_payload(op_id), notes.to_vec())
        .await
    {
        Ok(prepared) => Some(prepared),
        Err(error) => {
            warn!(%error, "multi-sig pin prepare failed");
            None
        }
    }
}

async fn prepare_withdraw<Node>(
    sequencer: &mut ZoneSequencer<Node>,
    op_id: &Hash,
    withdraw_outputs: &[u64],
    recipient: ZkPublicKey,
) -> Option<PreparedAtomicBundle>
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    let outputs: Vec<Note> = withdraw_outputs
        .iter()
        .map(|&value| Note::new(value, recipient))
        .collect();
    let outputs = Outputs::try_new(outputs).ok()?;
    match sequencer
        .handle()
        .prepare_atomic_withdraw(
            withdraw_payload(op_id),
            vec![WithdrawArg { outputs }],
            WithdrawInputs::Auto,
        )
        .await
    {
        Ok(prepared) => Some(prepared),
        Err(error) => {
            warn!(%error, "multi-sig withdraw prepare failed");
            None
        }
    }
}

/// Record our prepared bundle on the bus, mark it ours, and announce it.
fn open_round(
    bus: &MultiSigBus,
    announce: &BundleAnnounce,
    mine: &mut HashMap<(Hash, MultiSigPhase), BundleId>,
    work: (Hash, MultiSigPhase),
    prepared: PreparedAtomicBundle,
) {
    let id = prepared.sign_payload.clone();
    let prepared = Arc::new(prepared);
    bus.lock().unwrap().insert(
        id.clone(),
        MultiSigRound {
            prepared: Arc::clone(&prepared),
            signatures: IndexedSignatures::empty(),
            op_id: work.0,
        },
    );
    mine.insert(work, id);
    // Insert before announcing so every signer sees the round. Best-effort:
    // errors only once all signer tasks have exited (teardown).
    drop(announce.send(prepared));
}

/// Forget our bundle for `work` so the next event prepares a fresh one.
fn close_round(
    bus: &MultiSigBus,
    mine: &mut HashMap<(Hash, MultiSigPhase), BundleId>,
    work: &(Hash, MultiSigPhase),
) {
    if let Some(id) = mine.remove(work) {
        bus.lock().unwrap().remove(&id);
    }
}

/// Submit the bundle once it has a threshold of signatures: `Ok(true)` when
/// submitted, `Ok(false)` while signatures are still being gathered. No turn
/// check: the SDK holds the posted tx until our own write turn.
fn submit_ready<Node>(
    sequencer: &mut ZoneSequencer<Node>,
    bus: &MultiSigBus,
    id: &BundleId,
) -> Result<bool, lb_zone_sdk::sequencer::Error>
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    let ready = {
        let bus = bus.lock().unwrap();
        bus.get(id)
            .filter(|round| round.signatures.len() >= usize::from(round.prepared.signing_threshold))
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

impl MultiSigLifecyclePolicy {
    async fn on_event<Node>(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event)
    where
        Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
    {
        let Event::BlocksProcessed {
            channel_update,
            deposits: observed,
            finalized: finalized_txs,
            ..
        } = event
        else {
            return;
        };
        let Self {
            withdraw_outputs,
            recipient,
            bus,
            announce,
            deposits,
            finalized,
            mine,
            ..
        } = self;

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
        // An extension only adds steps; a conflict recomputes them from the view.
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

        let wallet: HashSet<NoteId> = {
            let view = sequencer.channel_wallet();
            view.finalized
                .iter()
                .chain(view.unfinalized.iter())
                .map(|note| note.note_id)
                .collect()
        };
        // Drive off canonical state: pin while the note is on-branch and
        // unpinned, then withdraw once pinned and the note is consumed. "Drive" =
        // prepare our own bundle if we have none, else submit it and set the flag
        // optimistically; the reconcile above clears it on a conflict so we drive
        // again. A refused submit drops the bundle so the next event re-prepares.
        for (op_id, state) in deposits.iter_mut() {
            if state.notes.is_empty() {
                continue;
            }
            let on_branch = state.notes.iter().all(|id| wallet.contains(id));
            let phase = if !state.pinned && on_branch {
                MultiSigPhase::Pin
            } else if !state.withdrawn && state.pinned && !on_branch {
                MultiSigPhase::Withdraw
            } else {
                continue;
            };
            let work = (*op_id, phase);
            let Some(id) = mine.get(&work) else {
                let prepared = match phase {
                    MultiSigPhase::Pin => prepare_pin(sequencer, op_id, &state.notes).await,
                    MultiSigPhase::Withdraw => {
                        prepare_withdraw(sequencer, op_id, withdraw_outputs, *recipient).await
                    }
                };
                if let Some(prepared) = prepared {
                    open_round(bus, announce, mine, work, prepared);
                }
                continue;
            };
            match submit_ready(sequencer, bus, id) {
                Ok(true) => match phase {
                    MultiSigPhase::Pin => state.pinned = true,
                    MultiSigPhase::Withdraw => state.withdrawn = true,
                },
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, "multi-sig bundle submit failed");
                    close_round(bus, mine, &work);
                }
            }
        }

        // Drop a deposit's bundles once it is fully withdrawn.
        let withdrawn: HashSet<Hash> = deposits
            .iter()
            .filter(|(_, state)| state.withdrawn)
            .map(|(op_id, _)| *op_id)
            .collect();
        if !withdrawn.is_empty() {
            bus.lock()
                .unwrap()
                .retain(|_, round| !withdrawn.contains(&round.op_id));
            mine.retain(|(op_id, _), _| !withdrawn.contains(op_id));
        }
    }

    /// Sign an announced bundle once with our key and record it on the bus.
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
}

/// Single-phase sibling of [`start_deposit_lifecycle_policy`]: reactively
/// withdraw the deposit of `target_amount`, `Auto` sweeping other notes.
pub fn start_deposit_withdraw_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    target_amount: u64,
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
) -> PolicyRuntime {
    let policy = DepositWithdrawPolicy {
        target_amount,
        withdraw_outputs,
        recipient,
        deposits: HashMap::new(),
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

struct DepositWithdrawState {
    notes: Vec<NoteId>,
    withdraw_tx: Option<InscriptionId>,
}

/// Reactively withdraw the deposit of `target_amount` (no pin step),
/// reconciled against branch state; the withdraw consumes the deposit note
/// directly, so a foreign withdraw removes it and we back off.
struct DepositWithdrawPolicy {
    target_amount: u64,
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    deposits: HashMap<Hash, DepositWithdrawState>,
}

/// On a conflict, forget a withdraw that is neither in the view nor
/// finalized: it was shed. Nothing leaves on an extension.
fn retain_live_withdraws(
    deposits: &mut HashMap<Hash, DepositWithdrawState>,
    channel_update: &ChannelUpdate,
    finalized: &[FinalizedTx],
) {
    let Some(chain) = channel_update.canonical_chain() else {
        return;
    };
    let live: HashSet<InscriptionId> = chain
        .map(ChannelUpdateTx::tx_hash)
        .chain(finalized.iter().map(|tx| tx.tx_hash))
        .collect();
    for state in deposits.values_mut() {
        state.withdraw_tx = state.withdraw_tx.filter(|hash| live.contains(hash));
    }
}

impl<Node> runner::Policy<Node> for DepositWithdrawPolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let Event::BlocksProcessed {
            channel_update,
            deposits: observed,
            finalized,
            ..
        } = event
        else {
            return;
        };

        let Self {
            target_amount,
            withdraw_outputs,
            recipient,
            deposits,
        } = self;

        for deposit in observed {
            if deposit.amount == *target_amount {
                deposits
                    .entry(deposit.op_id)
                    .or_insert_with(|| DepositWithdrawState {
                        notes: deposit.notes.iter().map(|note| note.note_id).collect(),
                        withdraw_tx: None,
                    });
            }
        }
        retain_live_withdraws(deposits, channel_update, finalized);

        let wallet: HashSet<NoteId> = {
            let view = sequencer.channel_wallet();
            view.finalized
                .iter()
                .chain(view.unfinalized.iter())
                .map(|note| note.note_id)
                .collect()
        };

        for (op_id, state) in deposits.iter_mut() {
            let deposit_on_branch =
                !state.notes.is_empty() && state.notes.iter().all(|id| wallet.contains(id));
            if state.withdraw_tx.is_none() && deposit_on_branch {
                let inscription = make_inscription(&format!("withdraw deposit {op_id:?}"));
                state.withdraw_tx =
                    publish_deposit_withdraw(sequencer, inscription, withdraw_outputs, *recipient)
                        .await;
            }
        }
    }
}
