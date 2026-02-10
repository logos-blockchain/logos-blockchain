use std::{
    collections::HashSet,
    marker::PhantomData,
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use lb_core::{
    block::Block,
    mantle::{AuthenticatedMantleTx as _, SignedMantleTx},
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_node::HeaderId;
use testing_framework_core::scenario::{DynError, Expectation, RunContext};
use thiserror::Error;
use tokio::{sync::broadcast, time::sleep};

use super::workload::{SubmissionPlan, limited_user_count, submission_plan};
use crate::{TopologyConfig, framework::LbcEnv, workloads::LbcBlockFeedEnv};

const MIN_INCLUSION_RATIO: f64 = 0.5;
const CATCHUP_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_CATCHUP_WAIT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct TxInclusionExpectation<E = LbcEnv> {
    txs_per_block: NonZeroU64,
    user_limit: Option<NonZeroUsize>,
    capture_state: Option<CaptureState>,
    _env: PhantomData<fn() -> E>,
}

#[derive(Clone)]
struct CaptureState {
    observed: Arc<AtomicU64>,
    expected: u64,
}

#[derive(Debug, Error)]
enum TxExpectationError {
    #[error("transaction workload requires seeded accounts")]
    MissingAccounts,
    #[error("transaction workload planned zero transactions")]
    NoPlannedTransactions,
    #[error("transaction inclusion expectation not captured")]
    NotCaptured,
    #[error("transaction inclusion observed {observed} below required {required}")]
    InsufficientInclusions { observed: u64, required: u64 },
}

impl<E> TxInclusionExpectation<E> {
    pub const NAME: &'static str = "tx_inclusion_expectation";

    #[must_use]
    pub const fn new(txs_per_block: NonZeroU64, user_limit: Option<NonZeroUsize>) -> Self {
        Self {
            txs_per_block,
            user_limit,
            capture_state: None,
            _env: PhantomData,
        }
    }
}

#[async_trait]
impl<E> Expectation<E> for TxInclusionExpectation<E>
where
    E: LbcBlockFeedEnv,
{
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn start_capture(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        if self.capture_state.is_some() {
            return Ok(());
        }

        let (plan, tracked_accounts) = build_capture_plan(self, ctx)?;
        if plan.transaction_count == 0 {
            return Err(TxExpectationError::NoPlannedTransactions.into());
        }

        tracing::info!(
            planned_txs = plan.transaction_count,
            txs_per_block = self.txs_per_block.get(),
            user_limit = self.user_limit.map(NonZeroUsize::get),
            "tx inclusion expectation starting capture"
        );

        let observed = Arc::new(AtomicU64::new(0));
        let mut receiver = E::block_feed_subscription(ctx);
        let tracked_accounts = Arc::new(tracked_accounts);
        let captured_observed = Arc::clone(&observed);

        tokio::spawn(async move {
            let genesis_parent = HeaderId::from([0; 32]);
            tracing::debug!("tx inclusion capture task started");

            loop {
                match receiver.recv().await {
                    Ok(record) => {
                        if record.block.header().parent() == genesis_parent {
                            continue;
                        }

                        capture_tx_outputs(
                            record.block.as_ref(),
                            tracked_accounts.as_ref(),
                            captured_observed.as_ref(),
                        );
                    }

                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::debug!(skipped, "tx inclusion capture lagged");
                    }

                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!("tx inclusion capture feed closed");
                        break;
                    }
                }
            }

            tracing::debug!("tx inclusion capture task exiting");
        });

        self.capture_state = Some(CaptureState {
            observed,
            expected: plan.transaction_count as u64,
        });

        Ok(())
    }

    async fn evaluate(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        let state = self
            .capture_state
            .as_ref()
            .ok_or(TxExpectationError::NotCaptured)?;

        let required = ((state.expected as f64) * MIN_INCLUSION_RATIO).ceil() as u64;
        let observed = wait_for_required_inclusions(state, required, ctx).await;
        report_inclusion_result(observed, required, state.expected)
    }
}

fn report_inclusion_result(observed: u64, required: u64, expected: u64) -> Result<(), DynError> {
    if observed < required {
        tracing::warn!(
            observed,
            required,
            expected,
            "tx inclusion expectation failed"
        );

        return Err(TxExpectationError::InsufficientInclusions { observed, required }.into());
    }

    tracing::info!(
        observed,
        required,
        expected,
        "tx inclusion expectation satisfied"
    );

    Ok(())
}

async fn wait_for_required_inclusions<E: LbcBlockFeedEnv>(
    state: &CaptureState,
    required: u64,
    ctx: &RunContext<E>,
) -> u64 {
    let mut observed = state.observed.load(Ordering::Relaxed);
    if observed >= required {
        return observed;
    }

    let mut remaining = catchup_wait_budget(ctx);
    while observed < required && remaining > Duration::ZERO {
        sleep(CATCHUP_POLL_INTERVAL).await;
        remaining = remaining.saturating_sub(CATCHUP_POLL_INTERVAL);
        observed = state.observed.load(Ordering::Relaxed);
    }

    observed
}

fn build_capture_plan<E: LbcBlockFeedEnv>(
    expectation: &TxInclusionExpectation<E>,
    ctx: &RunContext<E>,
) -> Result<(SubmissionPlan, HashSet<ZkPublicKey>), DynError> {
    let wallet_accounts = ctx.descriptors().config().wallet_config.accounts.clone();
    if wallet_accounts.is_empty() {
        return Err(TxExpectationError::MissingAccounts.into());
    }

    let available = limited_user_count(expectation.user_limit, wallet_accounts.len());
    let plan = submission_plan(expectation.txs_per_block, ctx, available)?;

    let wallet_pks = wallet_accounts
        .into_iter()
        .take(plan.transaction_count)
        .map(|account| account.secret_key.to_public_key())
        .collect::<HashSet<_>>();

    Ok((plan, wallet_pks))
}

fn block_interval_hint<E: LbcBlockFeedEnv>(ctx: &RunContext<E>) -> Option<Duration> {
    let config: &TopologyConfig = ctx.descriptors().config();
    let slot = config.slot_duration?;
    let coeff = config.active_slot_coeff.clamp(0.0, 1.0);
    Some(slot.mul_f64(coeff))
}

fn catchup_wait_budget<E: LbcBlockFeedEnv>(ctx: &RunContext<E>) -> Duration {
    let security_param = ctx.descriptors().config().security_param;
    let hinted_wait =
        block_interval_hint(ctx).map(|interval| interval.mul_f64(f64::from(security_param)));

    hinted_wait
        .unwrap_or(MAX_CATCHUP_WAIT)
        .min(MAX_CATCHUP_WAIT)
}

fn capture_tx_outputs(
    block: &Block<SignedMantleTx>,
    tracked_accounts: &HashSet<ZkPublicKey>,
    observed: &AtomicU64,
) {
    for tx in block.transactions() {
        for note in &tx.mantle_tx().ledger_tx.outputs {
            if tracked_accounts.contains(&note.pk) {
                observed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(pk = ?note.pk, "tx inclusion observed account output");
                break;
            }
        }
    }
}
