use std::time::Duration;

use lb_core::mantle::{
    TxHash, Value,
    ledger::Inputs,
    ops::channel::{ChannelId, deposit::Metadata, withdraw::ChannelWithdrawOp},
};
use lb_zone_sdk::{
    adapter::NodeHttpClient,
    sequencer::{Event, FinalizedOp, FinalizedTx, TxStatus, TxStatusUpdate, ZoneSequencer},
};
use tokio::{select, sync::broadcast, time::sleep};

use crate::{
    cli::RunResult,
    run_commands::utils::{save_cli_checkpoint, timestamp},
};

const COMMAND_FINALITY_TIMEOUT: Duration = Duration::from_mins(5);

/// Command verification depth.
#[derive(Clone, Copy)]
pub enum WaitFor {
    /// Return once the transaction is observed on the local canonical chain.
    OnChain,
    /// Return only once the command goal is finalized.
    Finalized,
}

/// Finalization target for a non-interactive sequencer command.
pub enum CommandGoal {
    /// Wait until the transaction hash appears in finalized chain history.
    Tx { tx_hash: TxHash },
    /// Wait until the expected deposit op appears in finalized channel ops.
    Deposit {
        /// Signed transaction hash that carried the deposit.
        tx_hash: TxHash,
        /// Deposit note inputs.
        inputs: Inputs,
        /// Deposited amount.
        amount: Value,
        /// Deposit metadata.
        metadata: Metadata,
    },
    /// Wait until all expected withdraw ops appear in the finalized tx.
    Withdraw {
        /// Signed transaction hash that carried the withdraw ops.
        tx_hash: TxHash,
        /// Expected withdraw ops.
        withdraws: Vec<ChannelWithdrawOp>,
    },
}

impl CommandGoal {
    const fn tx_hash(&self) -> TxHash {
        match self {
            Self::Tx { tx_hash }
            | Self::Deposit { tx_hash, .. }
            | Self::Withdraw { tx_hash, .. } => *tx_hash,
        }
    }
}

/// Drive a sequencer until the requested command goal reaches the wait target.
pub async fn drive_until_observed(
    channel_id: &ChannelId,
    sequencer: &mut ZoneSequencer<NodeHttpClient>,
    mut status_rx: broadcast::Receiver<TxStatusUpdate>,
    goal: CommandGoal,
    wait_for: WaitFor,
    label: &str,
) -> RunResult<()> {
    let tx_hash = goal.tx_hash();
    let timeout = sleep(COMMAND_FINALITY_TIMEOUT);
    tokio::pin!(timeout);
    let mut observed_on_chain = false;

    loop {
        select! {
            () = &mut timeout => {
                return Err(format!(
                    "{} {label}: verification timeout tx_hash={}",
                    timestamp(),
                    hex::encode(tx_hash.as_ref())
                ).into());
            }
            update = status_rx.recv() => match update {
                Ok(update) if update.tx_hash == tx_hash => {
                    print_status(label, update);
                    if matches!(update.status, TxStatus::Orphaned(_)) {
                        return Err(format!(
                            "{} {label}: orphaned tx_hash={}",
                            timestamp(),
                            hex::encode(tx_hash.as_ref())
                        ).into());
                    }
                    if matches!(update.status, TxStatus::OnChain(_))
                        && matches!(wait_for, WaitFor::OnChain)
                    {
                        observed_on_chain = true;
                    }
                    if matches!(update.status, TxStatus::Finalized(_))
                        && matches!(goal, CommandGoal::Tx { .. })
                    {
                        return Ok(());
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    println!(
                        "{} {label}: verification lagged skipped_updates={skipped}",
                        timestamp()
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("sequencer tx-status stream closed".into());
                }
            },
            event = sequencer.next_event() => {
                if let Event::BlocksProcessed { checkpoint, finalized, .. } = event {
                    save_cli_checkpoint(channel_id, &checkpoint)?;
                    if observed_on_chain {
                        return Ok(());
                    }
                    if finalized_goal_matches(&goal, &finalized) {
                        println!(
                            "{} {label}: finalized tx_hash={}",
                            timestamp(),
                            hex::encode(tx_hash.as_ref())
                        );
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn print_status(label: &str, update: TxStatusUpdate) {
    println!(
        "{} {label}: verification tx_hash={} status={:?}",
        timestamp(),
        hex::encode(update.tx_hash.as_ref()),
        update.status
    );
}

fn finalized_goal_matches(goal: &CommandGoal, finalized: &[FinalizedTx]) -> bool {
    finalized.iter().any(|tx| match goal {
        CommandGoal::Tx { tx_hash } => tx.tx_hash == *tx_hash,
        CommandGoal::Deposit {
            tx_hash,
            inputs,
            amount,
            metadata,
        } => {
            tx.tx_hash == *tx_hash
                && tx.ops.iter().any(|op| {
                    matches!(op, FinalizedOp::Deposit(deposit)
                        if deposit.inputs == *inputs
                            && deposit.amount == *amount
                            && deposit.metadata == *metadata)
                })
        }
        CommandGoal::Withdraw { tx_hash, withdraws } => {
            tx.tx_hash == *tx_hash
                && withdraws.iter().all(|expected| {
                    tx.ops.iter().any(|op| {
                        matches!(op, FinalizedOp::Withdraw(withdraw)
                            if withdraw.op.channel_id == expected.channel_id
                                && withdraw.op.outputs == expected.outputs
                                && withdraw.op.withdraw_nonce == expected.withdraw_nonce)
                    })
                })
        }
    })
}
