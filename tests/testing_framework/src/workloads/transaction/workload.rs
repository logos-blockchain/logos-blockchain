use std::{
    collections::{HashMap, VecDeque},
    marker::PhantomData,
    num::{NonZeroU64, NonZeroUsize},
    slice,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use lb_core::mantle::{
    GasCalculator as _, GenesisTx as _, Note, OpProof, SignedMantleTx, Transaction as _, Utxo,
    gas::MainnetGasConstants,
    genesis_tx::GENESIS_STORAGE_GAS_PRICE,
    ops::OpId as _,
    tx::{MantleTxContext, MantleTxGasContext},
    tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use rand::{seq::SliceRandom as _, thread_rng};
use testing_framework_core::scenario::{
    DynError, Expectation, RunContext, RunMetrics, Workload as ScenarioWorkload,
};
use thiserror::Error;
use tokio::time::sleep;
use tracing::debug;

use super::expectation::TxInclusionExpectation;
use crate::{
    framework::LbcEnv,
    node::{DeploymentPlan, NodeHttpClient, configs::wallet::WalletAccount},
    workloads::{LbcBlockFeedEnv, LbcScenarioEnv},
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
pub struct WorkloadImpl<E = LbcEnv> {
    txs_per_block: NonZeroU64,
    user_limit: Option<NonZeroUsize>,
    accounts: Vec<WalletInput>,
    _env: PhantomData<fn() -> E>,
}

#[derive(Clone)]
struct WalletInput {
    account: WalletAccount,
    utxo: Utxo,
}

pub type Workload<E = LbcEnv> = WorkloadImpl<E>;

#[async_trait]
impl<E> ScenarioWorkload<E> for WorkloadImpl<E>
where
    E: LbcScenarioEnv + LbcBlockFeedEnv,
{
    fn name(&self) -> &'static str {
        "tx_workload"
    }

    fn expectations(&self) -> Vec<Box<dyn Expectation<E>>> {
        vec![Box::new(TxInclusionExpectation::<E>::new(
            self.txs_per_block,
            self.user_limit,
        ))]
    }

    fn init(
        &mut self,
        descriptors: &DeploymentPlan,
        _run_metrics: &RunMetrics,
    ) -> Result<(), DynError> {
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
                .transactions()
                .next()
                .expect("Genesis block should contain a genesis tx"),
        );

        let mut accounts = wallet_accounts
            .into_iter()
            .filter_map(|account| {
                utxo_map
                    .get(&account.public_key())
                    .copied()
                    .map(|utxo| WalletInput { account, utxo })
            })
            .collect::<Vec<_>>();

        apply_user_limit(&mut accounts, self.user_limit);
        if accounts.is_empty() {
            return Err(TxWorkloadError::MissingWalletUtxos.into());
        }

        self.accounts = accounts;
        Ok(())
    }

    async fn start(&self, ctx: &RunContext<E>) -> Result<(), DynError> {
        Submission::new(self, ctx)?.execute().await
    }
}

impl<E> WorkloadImpl<E> {
    #[must_use]
    pub const fn new(txs_per_block: NonZeroU64) -> Self {
        Self {
            txs_per_block,
            user_limit: None,
            accounts: Vec::new(),
            _env: PhantomData,
        }
    }

    #[must_use]
    pub const fn with_user_limit(mut self, user_limit: Option<NonZeroUsize>) -> Self {
        self.user_limit = user_limit;
        self
    }
}

impl<E> Default for WorkloadImpl<E> {
    fn default() -> Self {
        Self::new(NonZeroU64::MIN)
    }
}

struct Submission<'a, E: LbcScenarioEnv> {
    plan: VecDeque<WalletInput>,
    ctx: &'a RunContext<E>,
    interval: Duration,
}

impl<'a, E: LbcScenarioEnv> Submission<'a, E> {
    fn new(workload: &WorkloadImpl<E>, ctx: &'a RunContext<E>) -> Result<Self, DynError> {
        if workload.accounts.is_empty() {
            return Err(TxWorkloadError::MissingPreparedAccounts.into());
        }

        let submission_plan =
            submission_plan(workload.txs_per_block, ctx, workload.accounts.len())?;
        let plan = workload
            .accounts
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
        let gas_context = MantleTxGasContext::new(HashMap::new());
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
    ctx: &RunContext<impl LbcScenarioEnv>,
    input: &WalletInput,
    gas_context: MantleTxGasContext,
) -> Result<(), DynError> {
    let signed_tx = Arc::new(build_wallet_transaction(input, &gas_context)?);
    submit_transaction_via_cluster(ctx, signed_tx).await
}

const SUBMIT_RETRIES: usize = 5;
const SUBMIT_RETRY_DELAY: Duration = Duration::from_millis(500);

async fn submit_transaction_via_cluster(
    ctx: &RunContext<impl LbcScenarioEnv>,
    tx: Arc<SignedMantleTx>,
) -> Result<(), DynError> {
    let tx_hash = tx.hash();
    debug!(?tx_hash, "submitting transaction via cluster (nodes first)");

    let mut clients = ctx.node_clients().snapshot();
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
    tx: &SignedMantleTx,
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
    gas_context: &MantleTxGasContext,
) -> Result<SignedMantleTx, DynError> {
    let receiver = input.account.public_key();
    let tx_context = MantleTxContext {
        gas_context: gas_context.clone(),
        leader_reward_amount: 0,
    };

    let provisional_tx = MantleTxBuilder::new(tx_context.clone())
        .set_execution_gas_price(0.into())
        .set_storage_gas_price(GENESIS_STORAGE_GAS_PRICE)
        .add_ledger_input(input.utxo)
        .add_ledger_output(Note::new(input.utxo.note.value, receiver))
        .build();

    let fee = provisional_tx
        .total_gas_cost::<MainnetGasConstants>(gas_context)?
        .into_inner();
    let output_value = input.utxo.note.value.checked_sub(fee).ok_or_else(|| {
        format!(
            "input note value {} below fee {}",
            input.utxo.note.value, fee
        )
    })?;

    let tx = MantleTxBuilder::new(tx_context)
        .set_execution_gas_price(0.into())
        .set_storage_gas_price(GENESIS_STORAGE_GAS_PRICE)
        .add_ledger_input(input.utxo)
        .add_ledger_output(Note::new(output_value, receiver))
        .build();

    let signature = ZkKey::multi_sign(
        slice::from_ref(&input.account.secret_key),
        &tx.hash().to_fr(),
    )
    .map_err(|err| format!("failed to sign transaction: {err}"))?;

    SignedMantleTx::new(tx, vec![OpProof::ZkSig(signature)])
        .map_err(|err| format!("failed to build signed transaction: {err}").into())
}

fn wallet_utxo_map(
    genesis_tx: &lb_core::mantle::genesis_tx::GenesisTx,
) -> HashMap<ZkPublicKey, Utxo> {
    let transfer_op = genesis_tx.genesis_transfer().clone();
    let op_id = transfer_op.op_id();

    transfer_op
        .outputs
        .iter()
        .enumerate()
        .map(|(idx, note)| (note.pk, Utxo::new(op_id, idx, *note)))
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

pub(super) fn submission_plan<E: LbcScenarioEnv>(
    txs_per_block: NonZeroU64,
    ctx: &RunContext<E>,
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
