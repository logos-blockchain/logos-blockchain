use super::{
    ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, Hash, HashMap, HashSet, Inscription,
    InscriptionId, Note, NoteId, Outputs, PolicyRuntime, WithdrawArg, WithdrawInputs, ZkPublicKey,
    ZoneNodeHttpClient, ZoneSequencer, make_inscription, runner, to_policy_runtime, warn,
};

/// Reactively drive the full deposit lifecycle (integrate, then withdraw the
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
    integrated: bool,
    withdrawn: bool,
}

/// Reconciles each observed deposit against branch state (fork- and
/// multi-sequencer-correct), matching phases by the deposit's `op_id`:
/// integrate it, then withdraw the re-created note.
struct DepositLifecyclePolicy {
    withdraw_outputs: Vec<u64>,
    recipient: ZkPublicKey,
    deposits: HashMap<Hash, DepositLifecycleState>,
}

fn integrate_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("integrate deposit {op_id:?}"))
}

fn withdraw_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("withdraw deposit {op_id:?}"))
}

fn mark_payload(
    deposits: &mut HashMap<Hash, DepositLifecycleState>,
    payload: &Inscription,
    present: bool,
) {
    for (op_id, state) in deposits.iter_mut() {
        if *payload == integrate_payload(op_id) {
            state.integrated = present;
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
        .publish_atomic_deposit_inscription(inscription, notes)
        .await
    {
        Ok((result, _)) => Some(result.inscription_id()),
        Err(error) => {
            warn!(%error, "deposit-integration inscription failed");
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
                    integrated: false,
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
            if !state.integrated && deposit_on_branch {
                let inscription = integrate_payload(op_id);
                if publish_deposit_inscription(sequencer, inscription, state.notes.clone())
                    .await
                    .is_some()
                {
                    state.integrated = true;
                }
            } else if !state.withdrawn && state.integrated && !deposit_on_branch {
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

/// Reactively withdraw the deposit of `target_amount` (no integration step),
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
