use std::{collections::HashSet, time::Duration};

use cucumber::{gherkin::Step, given, then, when};
use tokio::time::timeout;
use tracing::{info, warn};

use crate::{
    common::wallet::WalletUtxos,
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET,
            transactions::{
                drain_wallets::{drain_all_node_wallets, drain_node_wallet, drain_user_wallet},
                manual_control::{
                    execute_coin_splits_all_user_wallets,
                    execute_continuous_next_wallet_user_wallet,
                    execute_continuous_round_robin_user_wallets, log_wallet_balances,
                    parsing::ManualCommand, perform_manual_step_control,
                    verify_min_outputs_all_user_wallets,
                },
                tracked_transactions::{
                    submit_funded_transfer_transaction, submit_invalid_transfer_transaction,
                    submit_stateless_invalid_transfer_transaction,
                    transaction_is_not_included_in_seconds,
                    transaction_is_rejected_during_preverification,
                },
                utils,
                utils::{
                    WalletOutputState,
                    assert_tracked_wallet_fees_equal_sponsored_fee_account_spend,
                    create_and_submit_transaction, parse_wallet_output_state,
                    wait_for_wallet_output_state, wait_for_wallet_submitted_transactions_inclusion,
                },
            },
        },
        wallet::{
            submissions::create_and_submit_transaction_hashes_with_utxo_cache,
            sync::{WalletSendReadiness, wait_wallet_send_ready},
        },
        world::{CucumberWorld, WalletInfo, WalletType},
    },
    non_zero,
};

mod draining;
mod faucet;
mod submissions;
mod transfers;
mod wallets;
mod workloads;
