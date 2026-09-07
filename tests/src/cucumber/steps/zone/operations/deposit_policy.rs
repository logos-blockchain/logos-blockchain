use std::sync::{Arc, Mutex};

use lb_key_management_system_service::keys::Ed25519Key;
use lb_zone_sdk::sequencer::sign_prepared;
use tokio::sync::broadcast;

use super::{
    ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, Hash, HashMap, HashSet, IndexedSignature,
    Inscription, InscriptionId, Note, NoteId, Outputs, PolicyRuntime, PreparedAtomicBundle,
    WithdrawArg, WithdrawInputs, ZkPublicKey, ZoneNodeHttpClient, ZoneSequencer, make_inscription,
    runner, to_policy_runtime, warn,
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
}

pub fn pin_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("pin deposit {op_id:?}"))
}

pub fn withdraw_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("withdraw deposit {op_id:?}"))
}

fn mark_payload(
    deposits: &mut HashMap<Hash, DepositLifecycleState>,
    payload: &Inscription,
    present: bool,
) {
    for (op_id, state) in deposits.iter_mut() {
        if *payload == pin_payload(op_id) {
            state.pinned = present;
        } else if *payload == withdraw_payload(op_id) {
            state.withdrawn = present;
        }
    }
}

/// Set each phase flag to `present` for deposits whose payload appears in
/// `txs`.
fn apply_channel_txs(
    deposits: &mut HashMap<Hash, DepositLifecycleState>,
    txs: &[ChannelUpdateTx],
    present: bool,
) {
    for tx in txs {
        if let Some(payload) = tx.inscription().map(|info| &info.payload) {
            mark_payload(deposits, payload, present);
        }
    }
}

/// Mark finalized steps present — canonical, so never un-set.
fn apply_finalized(deposits: &mut HashMap<Hash, DepositLifecycleState>, finalized: &[FinalizedTx]) {
    for op in finalized.iter().flat_map(|tx| tx.ops.iter()) {
        if let FinalizedOp::Inscription(info) = op {
            mark_payload(deposits, &info.payload, true);
        }
    }
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
            finalized,
            ..
        } = event
        else {
            return;
        };

        let Self {
            withdraw_outputs,
            recipient,
            deposits,
        } = self;

        for deposit in &channel_update.adopted_deposits {
            deposits
                .entry(deposit.op_id)
                .or_insert_with(|| DepositLifecycleState {
                    notes: deposit.notes.iter().map(|note| note.note_id).collect(),
                    pinned: false,
                    withdrawn: false,
                });
        }
        // orphaned first, then adopted/finalized (canonical) which win.
        apply_channel_txs(deposits, &channel_update.orphaned, false);
        apply_channel_txs(deposits, &channel_update.adopted, true);
        apply_finalized(deposits, finalized);

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

/// Phase of a deposit's multi-sig lifecycle, keyed on the shared bus.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MultiSigPhase {
    Pin,
    Withdraw,
}

/// A bundle's identity on the bus: its own signing payload (the bundle tx
/// hash). Because the multi-sig signature covers the whole bundle — including
/// each proposer's own turn-gated inscription — every proposer's bundle is a
/// distinct transaction with a distinct payload, so the payload is its natural
/// identifier. No proposer tag needed.
type BundleId = Vec<u8>;

/// A proposed bundle plus the signatures gathered for it, addressed by its
/// [`BundleId`]. Lives on the shared [`MultiSigBus`].
pub struct MultiSigRound {
    prepared: Arc<PreparedAtomicBundle>,
    signatures: Vec<IndexedSignature>,
    /// Deposit this bundle belongs to — lets any sequencer prune it from the
    /// shared bus once the deposit is fully withdrawn.
    op_id: Hash,
}

/// Shared signature collection: every sequencer's signer task records its
/// signature for a proposed bundle here, and the proposer reads it back to
/// submit once a threshold is gathered.
pub type MultiSigBus = Arc<Mutex<HashMap<BundleId, MultiSigRound>>>;

/// Bundle-announcement channel: a proposer broadcasts each prepared bundle so
/// every sequencer's signer task can sign it, off the block-processing path.
/// The test's stand-in for the gossip a real zone uses to exchange signatures —
/// signing reacts to a peer's announcement, not to the local block event.
pub type BundleAnnounce = broadcast::Sender<Arc<PreparedAtomicBundle>>;

/// Multi-sig counterpart of [`start_deposit_lifecycle_policy`]: every sequencer
/// runs this same policy, with no turn logic at all. Each reacts to an observed
/// deposit by preparing *its own* pin (then withdraw) bundle and dropping it on
/// the shared `bus`; every accredited sequencer signs every bundle it sees
/// in-loop via `sign_prepared_bundle`; each submits *its own* bundle once
/// `transfer_threshold` signatures are collected. The SDK holds each submission
/// until that sequencer's write turn, so whichever turn lands first wins, and
/// note-consumption invalidates the losers — observed on the next block as
/// `pinned`/`withdrawn`, which stops everyone else.
///
/// Drives one task per sequencer with a `select!` over two inputs: the SDK
/// event stream (chain reactions) and the announcement bus (signing). A real
/// zone's sequencer has exactly this shape — chain and gossip multiplexed in a
/// single owner of the sequencer.
pub fn start_multisig_lifecycle_policy(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    bus: MultiSigBus,
    announce: BundleAnnounce,
    signing_key: Ed25519Key,
) -> PolicyRuntime {
    // Grab the SDK subscriptions before moving the sequencer into the task
    // (mirrors `runner::spawn`, which we bypass here to drive the extra
    // `select!` arm for the signing bus).
    let checkpoint_rx = sequencer.subscribe_checkpoint();
    let ready_rx = sequencer.subscribe_ready();
    let channel_view_rx = sequencer.subscribe_channel_view();
    let turn_to_write_rx = sequencer.subscribe_turn_to_write();
    let tx_status_rx = sequencer.subscribe_tx_status();
    let event_rx = sequencer.subscribe_events();
    let client = sequencer.client();

    let mut announcements = announce.subscribe();
    let mut policy = MultiSigLifecyclePolicy {
        withdraw_outputs,
        recipient,
        bus,
        announce,
        signing_key,
        signed: HashSet::new(),
        deposits: HashMap::new(),
        mine: HashMap::new(),
    };

    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // Chain reactions: reconcile canonical state, then drive.
                event = sequencer.next_event() => {
                    policy.on_event(&mut sequencer, &event).await;
                }
                // Signing bus: a peer (or we) announced a bundle — sign it now,
                // off the block path. Exits when every announcer has dropped.
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
        client,
        event_rx,
        checkpoint_rx,
        ready_rx,
        channel_view_rx,
        turn_to_write_rx,
        tx_status_rx,
    })
}

struct MultiSigLifecyclePolicy {
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    bus: MultiSigBus,
    /// Announces our proposed bundles to every sequencer (including us).
    announce: BundleAnnounce,
    /// This sequencer's key, for signing announced bundles.
    signing_key: Ed25519Key,
    /// Bundles we have already signed (sign each once).
    signed: HashSet<BundleId>,
    deposits: HashMap<Hash, DepositLifecycleState>,
    /// The bundle this sequencer itself proposed for each work item — the one
    /// it submits (its inscription is signed by, and turn-gated to,
    /// itself).
    mine: HashMap<(Hash, MultiSigPhase), BundleId>,
}

/// Prepare (fund + build) the pin bundle for a deposit; `None` on failure.
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

/// Prepare (fund + build) the atomic-withdraw bundle for a deposit; `None` on
/// failure.
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

/// Record a freshly prepared bundle on the shared bus, remember it as this
/// sequencer's own for the given work item, and announce it so every
/// sequencer's signer task signs it.
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
            signatures: Vec::new(),
            op_id: work.0,
        },
    );
    mine.insert(work, id);
    // Inserted before announcing, so every signer sees the round to record into.
    // Best-effort: `send` errors only once every signer task has exited (test
    // teardown), never during normal operation.
    drop(announce.send(prepared));
}

/// Submit the bundle identified by `id` if it has gathered a threshold of
/// signatures; `true` when submitted. No turn check: the SDK holds the posted
/// tx until this sequencer's own write turn (the only turn its inscription is
/// valid in), so we submit as soon as it is ready.
fn submit_ready<Node>(sequencer: &mut ZoneSequencer<Node>, bus: &MultiSigBus, id: &BundleId) -> bool
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    let ready = {
        let bus = bus.lock().unwrap();
        bus.get(id)
            .filter(|round| round.signatures.len() as u16 >= round.prepared.signing_threshold)
            .map(|round| (round.prepared.as_ref().clone(), round.signatures.clone()))
    };
    let Some((prepared, mut signatures)) = ready else {
        return false;
    };
    signatures.sort_unstable();
    signatures.truncate(prepared.signing_threshold as usize);
    match sequencer
        .handle()
        .submit_atomic_bundle(prepared, signatures)
    {
        Ok(_) => true,
        Err(error) => {
            warn!(%error, "multi-sig bundle submit failed");
            false
        }
    }
}

impl MultiSigLifecyclePolicy {
    async fn on_event<Node>(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event)
    where
        Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
    {
        let Event::BlocksProcessed {
            channel_update,
            finalized,
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
            mine,
            ..
        } = self;

        for deposit in &channel_update.adopted_deposits {
            deposits
                .entry(deposit.op_id)
                .or_insert_with(|| DepositLifecycleState {
                    notes: deposit.notes.iter().map(|note| note.note_id).collect(),
                    pinned: false,
                    withdrawn: false,
                });
        }
        apply_channel_txs(deposits, &channel_update.orphaned, false);
        apply_channel_txs(deposits, &channel_update.adopted, true);
        apply_finalized(deposits, finalized);

        let wallet: HashSet<NoteId> = {
            let view = sequencer.channel_wallet();
            view.finalized
                .iter()
                .chain(view.unfinalized.iter())
                .map(|note| note.note_id)
                .collect()
        };
        // Signing happens off this loop, in each sequencer's `run_bundle_signer`
        // task, reacting to bundle announcements rather than block events.

        // React to canonical state, no turn logic: while the deposit note is on
        // our branch and unpinned, drive the pin; once pinned and the note is
        // consumed, drive the withdraw. "Drive" = prepare *our own* bundle if we
        // have none yet, else submit it once it has a threshold of signatures
        // and mark the phase done optimistically. Reconcile above reverts the
        // flag if the phase orphans, so we drive again. Every sequencer does
        // this for its own bundle; the SDK posts each in its own turn and
        // note-consumption leaves exactly one winner.
        for (op_id, state) in deposits.iter_mut() {
            if state.notes.is_empty() {
                continue;
            }
            let on_branch = state.notes.iter().all(|id| wallet.contains(id));
            if !state.pinned && on_branch {
                let work = (*op_id, MultiSigPhase::Pin);
                if let Some(id) = mine.get(&work) {
                    if submit_ready(sequencer, bus, id) {
                        state.pinned = true;
                    }
                } else if let Some(prepared) = prepare_pin(sequencer, op_id, &state.notes).await {
                    open_round(bus, announce, mine, work, prepared);
                }
            } else if !state.withdrawn && state.pinned && !on_branch {
                let work = (*op_id, MultiSigPhase::Withdraw);
                if let Some(id) = mine.get(&work) {
                    if submit_ready(sequencer, bus, id) {
                        state.withdrawn = true;
                    }
                } else if let Some(prepared) =
                    prepare_withdraw(sequencer, op_id, withdraw_outputs, *recipient).await
                {
                    open_round(bus, announce, mine, work, prepared);
                }
            }
        }

        // Once a deposit is fully withdrawn, drop its bundles from the shared
        // bus (any sequencer's pass prunes them for everyone).
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

    /// Sign an announced bundle once with our own key and record the signature
    /// on the shared bus for the proposer to collect. Pure crypto — no chain
    /// state — so it runs on the signing-bus `select!` arm, not the block path.
    fn sign_announced(&mut self, prepared: &PreparedAtomicBundle) {
        let id = prepared.sign_payload.clone();
        if !self.signed.insert(id.clone()) {
            return;
        }
        match sign_prepared(
            &self.signing_key,
            &prepared.accredited_keys,
            &prepared.sign_payload,
        ) {
            Ok(signature) => {
                if let Some(round) = self.bus.lock().unwrap().get_mut(&id) {
                    round.signatures.push(signature);
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

fn drop_shed_withdraws(
    deposits: &mut HashMap<Hash, DepositWithdrawState>,
    orphaned: &[ChannelUpdateTx],
) {
    for tx in orphaned {
        let hash = tx.tx_hash();
        for state in deposits.values_mut() {
            if state.withdraw_tx == Some(hash) {
                state.withdraw_tx = None;
            }
        }
    }
}

impl<Node> runner::Policy<Node> for DepositWithdrawPolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let Event::BlocksProcessed { channel_update, .. } = event else {
            return;
        };

        let Self {
            target_amount,
            withdraw_outputs,
            recipient,
            deposits,
        } = self;

        for deposit in &channel_update.adopted_deposits {
            if deposit.amount == *target_amount {
                deposits
                    .entry(deposit.op_id)
                    .or_insert_with(|| DepositWithdrawState {
                        notes: deposit.notes.iter().map(|note| note.note_id).collect(),
                        withdraw_tx: None,
                    });
            }
        }
        drop_shed_withdraws(deposits, &channel_update.orphaned);

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
