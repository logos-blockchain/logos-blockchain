use std::{
    collections::{HashMap, VecDeque},
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use lb_core::mantle::{
    Note, OpProof, SignedOps,
    gas::{MainnetGasProfile, TxGasCalculator as _},
    ledger::verification_mode::StandardMode,
    note_set::CommitmentMmr,
    notes::derive_note_nullifier,
    ops::{ZkTransferProof, transfer::prove_transfer},
    traits::{GenesisTx as _, Hashable as _},
    transactions::{
        GasPrices, MantleTxBuilder, OpProofs, builder::FundingNote, states::Preverified,
        tx_list::ops::OpsGasContext,
    },
};
use lb_groth16::Fr;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_mmr::MerklePath;
use rand::{seq::SliceRandom as _, thread_rng};
use testing_framework_app::AppHostEnv;
use testing_framework_core::scenario::{
    DynError, Expectation, RunContext, Workload as ScenarioWorkload,
};
use thiserror::Error;
use tokio::time::sleep;
use tracing::debug;

use super::expectation::TxInclusionExpectation;
use crate::{
    DeploymentPlan, NodeHttpClient, config::wallet::WalletAccount, workloads::LbcRunContextExt as _,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct SubmissionPlan {
    pub transaction_count: usize,
    pub submission_interval: Duration,
}

const MAX_SUBMISSION_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
enum TxWorkloadError {
    #[error("transaction workload requires seeded wallet accounts")]
    MissingWalletAccounts,
    #[error("transaction workload requires at least one node")]
    MissingReferenceNode,
    #[error("transaction workload could not map wallet accounts to genesis UTXOs")]
    MissingWalletUtxos,
    #[error("transaction workload has no prepared accounts")]
    MissingPreparedAccounts,
    #[error("no accounts available for transaction scheduling")]
    MissingAccountsForScheduling,
    #[error("calculated zero transactions to submit")]
    ZeroTransactionsToSubmit,
    #[error("cluster client exhausted all nodes")]
    ClusterClientExhausted,
}

#[derive(Clone)]
pub struct WorkloadImpl {
    txs_per_block: NonZeroU64,
    user_limit: Option<NonZeroUsize>,
}

#[derive(Clone)]
struct WalletInput {
    account: WalletAccount,
    note: GenesisNote,
}

// A genesis note opening with its path to the genesis ledger set root.
#[derive(Clone)]
struct GenesisNote {
    note: Note,
    path: MerklePath,
    root: Fr,
}

pub type Workload = WorkloadImpl;

#[async_trait]
impl ScenarioWorkload<AppHostEnv> for WorkloadImpl {
    fn name(&self) -> &'static str {
        "tx_workload"
    }

    fn expectations(&self) -> Vec<Box<dyn Expectation<AppHostEnv>>> {
        vec![Box::new(TxInclusionExpectation::new(
            self.txs_per_block,
            self.user_limit,
        ))]
    }

    async fn start(&self, ctx: &RunContext<AppHostEnv>) -> Result<(), DynError> {
        let accounts = prepare_accounts(&ctx.lbc_deployment()?, self.user_limit)?;
        Submission::new(self, &accounts, ctx)?.execute().await
    }
}

/// Maps seeded wallet accounts to their genesis UTXOs for the deployed plan.
///
/// Node-count and wallet checks run here rather than during scenario
/// validation because the outer app-host deployment reports no nodes.
fn prepare_accounts(
    descriptors: &DeploymentPlan,
    user_limit: Option<NonZeroUsize>,
) -> Result<Vec<WalletInput>, DynError> {
    let wallet_accounts = descriptors.config().wallet_config.accounts.clone();
    if wallet_accounts.is_empty() {
        return Err(TxWorkloadError::MissingWalletAccounts.into());
    }

    let _reference_node = descriptors
        .nodes()
        .first()
        .ok_or(TxWorkloadError::MissingReferenceNode)?;
    let genesis_block = descriptors
        .config()
        .genesis_block
        .as_ref()
        .ok_or(TxWorkloadError::MissingReferenceNode)?;
    let utxo_map = wallet_utxo_map(
        genesis_block
            .transactions_iter()
            .next()
            .expect("Genesis block should contain a genesis tx"),
    );

    let mut accounts = wallet_accounts
        .into_iter()
        .filter_map(|account| {
            utxo_map
                .get(&account.public_key())
                .cloned()
                .map(|note| WalletInput { account, note })
        })
        .collect::<Vec<_>>();

    apply_user_limit(&mut accounts, user_limit);
    if accounts.is_empty() {
        return Err(TxWorkloadError::MissingWalletUtxos.into());
    }

    Ok(accounts)
}

impl WorkloadImpl {
    #[must_use]
    pub const fn new(txs_per_block: NonZeroU64) -> Self {
        Self {
            txs_per_block,
            user_limit: None,
        }
    }

    #[must_use]
    pub const fn with_user_limit(mut self, user_limit: Option<NonZeroUsize>) -> Self {
        self.user_limit = user_limit;
        self
    }
}

impl Default for WorkloadImpl {
    fn default() -> Self {
        Self::new(NonZeroU64::MIN)
    }
}

struct Submission<'a> {
    plan: VecDeque<WalletInput>,
    ctx: &'a RunContext<AppHostEnv>,
    interval: Duration,
}

impl<'a> Submission<'a> {
    fn new(
        workload: &WorkloadImpl,
        accounts: &[WalletInput],
        ctx: &'a RunContext<AppHostEnv>,
    ) -> Result<Self, DynError> {
        if accounts.is_empty() {
            return Err(TxWorkloadError::MissingPreparedAccounts.into());
        }

        let submission_plan = submission_plan(workload.txs_per_block, ctx, accounts.len())?;
        let plan = accounts
            .iter()
            .take(submission_plan.transaction_count)
            .cloned()
            .collect();

        Ok(Self {
            plan,
            ctx,
            interval: submission_plan.submission_interval,
        })
    }

    async fn execute(mut self) -> Result<(), DynError> {
        let gas_context = OpsGasContext::new(HashMap::new(), GasPrices::default());
        while let Some(input) = self.plan.pop_front() {
            submit_wallet_transaction(self.ctx, &input, gas_context.clone()).await?;
            if !self.interval.is_zero() {
                sleep(self.interval).await;
            }
        }
        Ok(())
    }
}

async fn submit_wallet_transaction(
    ctx: &RunContext<AppHostEnv>,
    input: &WalletInput,
    gas_context: OpsGasContext,
) -> Result<(), DynError> {
    let signed_tx = Arc::new(build_wallet_transaction(input, &gas_context)?);
    submit_transaction_via_cluster(ctx, signed_tx).await
}

const SUBMIT_RETRIES: usize = 5;
const SUBMIT_RETRY_DELAY: Duration = Duration::from_millis(500);

async fn submit_transaction_via_cluster(
    ctx: &RunContext<AppHostEnv>,
    tx: Arc<SignedOps<Preverified, StandardMode>>,
) -> Result<(), DynError> {
    let tx_hash = tx.hash();
    debug!(?tx_hash, "submitting transaction via cluster (nodes first)");

    let mut clients = ctx.lbc_clients()?;
    if clients.is_empty() {
        return Err(cluster_client_exhausted_error());
    }

    clients.shuffle(&mut thread_rng());

    for attempt in 0..SUBMIT_RETRIES {
        match submit_to_clients(&mut clients, tx.as_ref(), attempt).await {
            Ok(()) => return Ok(()),
            Err(error) if !has_submission_retry(attempt) => return Err(error),
            Err(_) => sleep(SUBMIT_RETRY_DELAY).await,
        }
    }

    Err(cluster_client_exhausted_error())
}

const fn has_submission_retry(attempt: usize) -> bool {
    attempt + 1 < SUBMIT_RETRIES
}

async fn submit_to_clients(
    clients: &mut [NodeHttpClient],
    tx: &SignedOps<Preverified, StandardMode>,
    attempt: usize,
) -> Result<(), DynError> {
    let tx_hash = tx.hash();
    clients.shuffle(&mut thread_rng());
    let mut last_error = None;

    for client in clients {
        let url = client.base_url().clone();
        debug!(?tx_hash, %url, attempt, "submitting transaction to client");

        match client.submit_transaction(tx).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                debug!(?tx_hash, %url, attempt, "transaction submission failed");
                last_error = Some(error.into());
            }
        }
    }

    Err(last_error.unwrap_or_else(cluster_client_exhausted_error))
}

fn cluster_client_exhausted_error() -> DynError {
    TxWorkloadError::ClusterClientExhausted.into()
}

fn build_wallet_transaction(
    input: &WalletInput,
    gas_context: &OpsGasContext,
) -> Result<SignedOps<Preverified, StandardMode>, DynError> {
    let receiver = input.account.public_key();
    let secret_key = *input.account.secret_key.as_fr();
    let funding = FundingNote {
        note: input.note.note,
        nullifier: derive_note_nullifier(&input.note.note.commitment(), &secret_key),
    };
    let self_transfer = |value| {
        MantleTxBuilder::new()
            .extend_funding_inputs(input.note.root, [funding])
            .map_err(|err| format!("failed to add input: {err}"))?
            .add_ledger_output(Note::new(value, random_nonce(), receiver))
            .map_err(|err| format!("failed to add output: {err}"))
    };

    let provisional_tx = self_transfer(input.note.note.value)?
        .build()
        .map_err(|err| format!("failed to build provisional tx: {err}"))?;

    let fee = provisional_tx
        .by_ref()
        .total_gas_cost::<MainnetGasProfile>(gas_context)?
        .into_inner();
    let output_value = input.note.note.value.checked_sub(fee).ok_or_else(|| {
        format!(
            "input note value {} below fee {}",
            input.note.note.value, fee
        )
    })?;

    let tx_builder = self_transfer(output_value)?;
    let openings = tx_builder.openings();
    let tx = tx_builder
        .build()
        .map_err(|err| format!("failed to build tx: {err}"))?;
    let outputs = openings
        .into_values()
        .next()
        .map(|opening| opening.outputs)
        .unwrap_or_default();

    let proof = prove_transfer(
        &[(input.note.note, secret_key, input.note.path.clone())],
        &outputs,
        input.note.root,
        tx.hash().to_fr(),
    );

    let op_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(proof))]);
    SignedOps::from_parts(tx, op_proofs)
        .map_err(|err| format!("failed to build signed transaction: {err}"))?
        .preverify()
        .map_err(|err| format!("failed to build signed transaction: {err}").into())
}

fn random_nonce() -> Fr {
    Fr::from(rand::random::<u128>())
}

// The opening of a genesis allocation is named by its index; its path leads to
// the genesis ledger set root.
fn wallet_utxo_map(
    genesis_tx: &lb_core::mantle::transactions::GenesisTx,
) -> HashMap<ZkPublicKey, GenesisNote> {
    let notes = genesis_tx
        .distribution()
        .iter()
        .enumerate()
        .map(|(index, allocation)| allocation.note(index))
        .collect::<Vec<_>>();
    let mut mmr = CommitmentMmr::new();
    let mut paths = Vec::with_capacity(notes.len());
    for note in &notes {
        let (next, path) = mmr
            .push_with_paths(note.commitment(), &mut paths)
            .expect("genesis notes fit in the commitment MMR");
        mmr = next;
        paths.push(path);
    }
    let root = mmr.frontier_root();

    notes
        .into_iter()
        .zip(paths)
        .map(|(note, path)| (note.public_key, GenesisNote { note, path, root }))
        .collect()
}

fn apply_user_limit<T>(items: &mut Vec<T>, user_limit: Option<NonZeroUsize>) {
    if let Some(limit) = user_limit {
        items.truncate(limit.get().min(items.len()));
    }
}

pub(super) fn limited_user_count(user_limit: Option<NonZeroUsize>, available: usize) -> usize {
    user_limit.map_or(available, |limit| limit.get().min(available))
}

pub(super) fn submission_plan(
    txs_per_block: NonZeroU64,
    ctx: &RunContext<AppHostEnv>,
    available_accounts: usize,
) -> Result<SubmissionPlan, DynError> {
    if available_accounts == 0 {
        return Err(TxWorkloadError::MissingAccountsForScheduling.into());
    }

    let run_secs = ctx.run_duration().as_secs_f64();
    let target_transaction_count = (run_secs * txs_per_block.get() as f64)
        .floor()
        .clamp(0.0, u64::MAX as f64) as u64;

    let actual_transactions_to_submit =
        target_transaction_count.min(available_accounts as u64) as usize;
    if actual_transactions_to_submit == 0 {
        return Err(TxWorkloadError::ZeroTransactionsToSubmit.into());
    }

    let mut submission_interval =
        Duration::from_secs_f64(run_secs / actual_transactions_to_submit as f64);
    if submission_interval > MAX_SUBMISSION_INTERVAL {
        submission_interval = MAX_SUBMISSION_INTERVAL;
    }

    Ok(SubmissionPlan {
        transaction_count: actual_transactions_to_submit,
        submission_interval,
    })
}
