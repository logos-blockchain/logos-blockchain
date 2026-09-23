use super::{
    ChannelUpdate, ChannelUpdateTx, Event, FinalizedTx, Hash, HashMap, HashSet, Inscription,
    InscriptionId, Inscriptions as _, Note, NoteId, Outputs, PolicyRuntime, WithdrawArg,
    WithdrawInputs, ZkPublicKey, ZoneNodeHttpClient, ZoneSequencer, contributed,
    finalized_inscriptions, make_inscription, runner, to_policy_runtime, warn,
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

fn pin_payload(op_id: &Hash) -> Inscription {
    make_inscription(&format!("pin deposit {op_id:?}"))
}

fn withdraw_payload(op_id: &Hash) -> Inscription {
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
