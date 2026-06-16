use std::{collections::HashSet, num::NonZero};

use hex::ToHex as _;
use lb_core::{codec::SerializeOp as _, mantle::TxHash};
use tracing::warn;

pub use crate::cucumber::wallet::{
    WalletOutputState,
    checks::{
        assert_tracked_wallet_fees_equal_sponsored_fee_account_spend, wait_for_wallet_output_state,
    },
    parse_wallet_output_state,
    submissions::{
        create_and_submit_transaction, create_and_submit_transaction_hashes,
        create_and_submit_transaction_hashes_with_utxo_cache, wait_for_transactions_inclusion,
        wait_for_wallet_submitted_transactions_inclusion,
    },
    sync::{
        current_available_utxos_for_all_wallets, current_available_utxos_for_funding_wallets,
        current_available_utxos_for_user_wallets, current_available_utxos_for_wallet,
        current_wallet_available_state, current_wallet_balance,
    },
};
pub(crate) use crate::cucumber::wallet::{
    best_node::BestNodeInfo,
    submissions::{
        prepare_user_wallet_transaction_submission, submit_prepared_user_wallet_transaction,
    },
};
use crate::cucumber::{
    error::{StepError, StepResult},
    steps::{TARGET, manual_transactions::faucet::FaucetTask},
    world::CucumberWorld,
};

pub(crate) fn request_faucet_funds(
    world: &mut CucumberWorld,
    step: &str,
    number_of_rounds: NonZero<usize>,
    wallets: &[String],
) -> StepResult {
    if world.faucet_base_url.is_none()
        || world.faucet_username.is_none()
        || world.faucet_password.is_none()
    {
        warn!(
            target: TARGET,
            "Step `{}` error: Faucet details not set.",
            step
        );
        return Err(StepError::LogicalError {
            message: "Faucet details not set".to_owned(),
        });
    }
    let faucet_task = FaucetTask::new(
        world
            .faucet_base_url
            .clone()
            .expect("checked above")
            .as_ref(),
        world
            .faucet_username
            .clone()
            .expect("checked above")
            .as_ref(),
        world
            .faucet_password
            .clone()
            .expect("checked above")
            .as_ref(),
        wallets,
        number_of_rounds,
    );
    if let Some(handles) = &mut world.faucet_task_handles {
        handles.push(faucet_task.spawn(1000, step));
    } else {
        world.faucet_task_handles = Some(vec![faucet_task.spawn(1000, step)]);
    }

    Ok(())
}

pub(crate) fn tx_hash_hex(tx_hash: TxHash) -> String {
    tx_hash
        .to_bytes()
        .expect("is valid")
        .to_ascii_lowercase()
        .encode_hex::<String>()
}

pub(crate) fn extend_tx_hash_set<'a, I>(
    hash_set: &mut HashSet<TxHash>,
    hashes: I,
) -> Result<(), StepError>
where
    I: IntoIterator<Item = &'a TxHash>,
{
    for hash in hashes {
        if !hash_set.insert(*hash) {
            return Err(StepError::LogicalError {
                message: format!("Duplicate transaction hash: {}", tx_hash_hex(*hash)),
            });
        }
    }
    Ok(())
}
