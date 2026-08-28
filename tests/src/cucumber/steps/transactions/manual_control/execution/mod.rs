//! This module executes manual commands for Cucumber scenarios.
//!
//! External command controller:
//! - Set `CUCUMBER_MANUAL_COMMAND_FILE=/tmp/cucumber-manual-commands.txt`.
//! - Start the scenario.
//! - Prepare the command file beforehand, or append commands while the test
//!   runs.
//!
//! Supported commands (one per line):
//!
//! ```text
//! COIN_SPLIT, wallet '<wallet_name>', outputs <count>, value <amount>
//! VERIFY, wallet '<wallet_name>', outputs <count>, time_out <duration_seconds>
//! BALANCE, wallet '<wallet_name>'
//! EXPORT_FUNDS, wallet '<wallet_name>', value <amount>, output '<path>', include_secret true|false
//! BALANCE_ALL_WALLETS
//! BALANCE_ALL_USER_WALLETS
//! BALANCE_ALL_FUNDING_WALLETS
//! CLEAR_ENCUMBRANCES, wallet '<wallet_name>'
//! CLEAR_ENCUMBRANCES_ALL_WALLETS
//! SEND, num_transactions <count>, value <amount>, from '<wallet_name>', to '<wallet_name>'
//! DRAIN, from '<wallet_name>', to '<wallet_name>'
//! DRAIN_ALL_NODE_WALLETS, node_name '<node_name>', to '<wallet_name>'
//! VERIFY_MAX, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
//! VERIFY_MIN, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
//! CONTINUOUS_ROUND_ROBIN_USER_WALLETS, coin_split_outputs <count>, coin_split_value <amount>, num_transactions <count>, value <amount>, cycles <count>, epochs_headroom <count>
//! COIN_SPLIT_ALL_USER_WALLETS, splits_per_wallet <count>, outputs <count>, value <amount>
//! VERIFY_MIN_AVAILABLE_OUTPUTS_ALL_USER_WALLETS, min_outputs <count>, timeout_seconds <duration_seconds>
//! CONTINUOUS_NEXT_WALLET_USER_WALLETS, cycles <count>, num_transactions <count>, value <amount>, epochs_headroom <count>
//! FAUCET_ALL_USER_WALLETS, rounds <count>
//! FAUCET_ALL_FUNDING_WALLETS, rounds <count>
//! CREATE_SNAPSHOT_ALL_NODES, snapshot_name '<snapshot_name>'
//! CREATE_SNAPSHOT_NODE, snapshot_name '<snapshot_name>', node_name '<node_name>'
//! RESTART_NODE, node_name '<node_name>'
//! CRYPTARCHIA_INFO_ALL_NODES
//! WAIT_ALL_NODES_SYNCED_TO_CHAIN
//! STOP
//! ```

use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    hash::BuildHasher,
    num::NonZero,
    path::Path,
    time::Duration,
};

use lb_core::mantle::{
    NoteId, Utxo,
    transactions::{GasPrices, hash::TxHash},
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_wallet::WalletError;
use serde::Serialize;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    common::wallet::{TransactionFeePolicy, WalletStateView, WalletUtxos, build_fee_horizon},
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET, nodes,
            nodes::{
                create_snapshot_all_nodes_with_wallet_state,
                create_snapshot_node_with_wallet_state, restart_node,
                wait_for_all_nodes_to_be_synced_to_chain,
            },
            transactions::{
                drain_wallets::{drain_all_node_wallets, drain_node_wallet, drain_user_wallet},
                manual_control::parsing::{ManualCommand, take_next_command},
                utils,
                utils::{BestNodeInfo, WalletOutputState, extend_note_id_set, extend_tx_hash_set},
            },
        },
        wallet::{
            best_node::get_best_node_info,
            checks::wait_for_observed_transaction_hashes,
            submissions::{SignedUserWalletSubmission, validate_fee_horizon_after_wallet_batch},
            sync,
            sync::{WalletSendReadiness, current_available_utxos_for_user_wallets},
        },
        world::{CucumberWorld, WalletInfo, WalletType},
    },
};

const MANUAL_COMMAND_FILE_ENV: &str = "CUCUMBER_MANUAL_COMMAND_FILE";
const MANUAL_COMMAND_POLL_INTERVAL_ENV: &str = "CUCUMBER_MANUAL_COMMAND_POLL_INTERVAL_MS";
const MAX_TEST_EPOCH_HEADROOM: u32 = 16;

mod control;
mod dispatch;
mod fee_policy;
mod round_robin;
mod transactions;
mod wallet_state;

pub use control::perform_manual_step_control;
pub use dispatch::{
    execute_coin_splits_all_user_wallets, execute_continuous_next_wallet_user_wallet,
    execute_continuous_round_robin_user_wallets, execute_manual_command,
    verify_min_outputs_all_user_wallets,
};
use dispatch::{log_phase_counts, verify_transactions_mined};
pub use fee_policy::build_cycle_fee_policy;
use round_robin::{
    all_user_wallets, execute_continuous_round_robin, verify_no_duplicate_transactions,
};
use transactions::{
    execute_coin_split, execute_coin_split_with_utxo_cache, execute_send, handle_verify_command,
    prepare_coin_splits_all_wallets_with_utxo_cache, prepare_ring_send_round_send_with_utxo_cache,
    request_faucet_funds_all_funding_wallets, request_faucet_funds_all_user_wallets,
};
pub use wallet_state::log_wallet_balances;
use wallet_state::{
    clear_all_wallet_encumbrances, clear_wallet_encumbrances, execute_drain, export_funds,
    log_wallet_balance,
};
