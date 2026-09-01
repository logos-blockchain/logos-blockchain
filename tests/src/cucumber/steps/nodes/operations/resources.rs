use super::*;

#[must_use]
pub fn genesis_block_utxos(genesis_tx: &lb_core::mantle::transactions::GenesisTx) -> Vec<Utxo> {
    let transfer_op = genesis_tx.genesis_transfer().clone();
    let transfer_id = transfer_op.op_id();

    transfer_op
        .outputs
        .iter()
        .enumerate()
        .map(|(idx, note)| Utxo::new(transfer_id, idx, *note))
        .collect()
}

const ACCOUNT_INDEX: &str = "account_index";
const ACCOUNT_INDEX_IDX_T1: usize = 0;
const TOKEN_COUNT: &str = "token_count";
const TOKEN_COUNT_IDX: usize = 1;
const TOKEN_AMOUNT: &str = "token_amount";
const TOKEN_AMOUNT_IDX: usize = 2;

pub fn verify_genesis_wallet_resources_table_indexes(
    table: &Table,
    step: &str,
) -> Result<(), StepError> {
    if table.rows.is_empty()
        || table.rows[0].len() != 3
        || table.rows[0][ACCOUNT_INDEX_IDX_T1] != ACCOUNT_INDEX
        || table.rows[0][TOKEN_COUNT_IDX] != TOKEN_COUNT
        || table.rows[0][TOKEN_AMOUNT_IDX] != TOKEN_AMOUNT
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Wallet resources table must have a header row with columns: \
                {ACCOUNT_INDEX}, {TOKEN_COUNT}, {TOKEN_AMOUNT}"
            ),
        });
    }
    // All wallet account indexes must be unique
    let wallet_accounts: HashSet<_> = table
        .rows
        .iter()
        .map(|row| &row[ACCOUNT_INDEX_IDX_T1])
        .collect();
    if wallet_accounts.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {ACCOUNT_INDEX} indexes found in the table"
            ),
        });
    }

    Ok(())
}

pub fn parse_genesis_wallet_tokens_row(
    step: &str,
    row: &[String],
) -> Result<(usize, usize, u64), StepError> {
    let account_index =
        row[ACCOUNT_INDEX_IDX_T1]
            .parse::<usize>()
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Step `{step}` error: {ACCOUNT_INDEX} must be a valid number"),
            })?;
    let token_count =
        row[TOKEN_COUNT_IDX]
            .parse::<usize>()
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Step `{step}` error: {TOKEN_COUNT} must be a valid number"),
            })?;
    let token_amount =
        row[TOKEN_AMOUNT_IDX]
            .parse::<u64>()
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Step `{step}` error: {TOKEN_AMOUNT} must be a valid number"),
            })?;
    Ok((account_index, token_count, token_amount))
}

const NODE_NAME: &str = "node_name";
const NODE_NAME_IDX: usize = 0;
const ACCOUNT_INDEX_IDX_T2: usize = 1;
const WALLET_NAME: &str = "wallet_name";
const WALLET_NAME_IDX: usize = 2;
const CONNECTED_TO: &str = "connected_to";
const CONNECTED_TO_IDX: usize = 3;

// Mining-node wallet-resources table adds an `is_mining_wallet` column between
// `wallet_name` and `connected_to`.
const IS_MINING_WALLET: &str = "is_mining_wallet";
const IS_MINING_WALLET_IDX: usize = 3;
const MINING_CONNECTED_TO_IDX: usize = 4;

pub fn verify_node_wallet_resources_table_indexes(
    table: &Table,
    step: &str,
) -> Result<(), StepError> {
    if table.rows.is_empty()
        || table.rows[0].len() != 4
        || table.rows[0][NODE_NAME_IDX] != NODE_NAME
        || table.rows[0][ACCOUNT_INDEX_IDX_T2] != ACCOUNT_INDEX
        || table.rows[0][WALLET_NAME_IDX] != WALLET_NAME
        || table.rows[0][CONNECTED_TO_IDX] != CONNECTED_TO
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Wallet resources table must have a header row with columns: {NODE_NAME}, {ACCOUNT_INDEX}, {WALLET_NAME}, {CONNECTED_TO}"
            ),
        });
    }
    // All wallet indexes must be unique
    let account_indexes: HashSet<_> = table
        .rows
        .iter()
        .map(|row| &row[ACCOUNT_INDEX_IDX_T2])
        .collect();
    if account_indexes.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {ACCOUNT_INDEX} indexes found in the table"
            ),
        });
    }
    // All wallet names must be unique
    let wallet_names: HashSet<_> = table.rows.iter().map(|row| &row[WALLET_NAME_IDX]).collect();
    if wallet_names.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {WALLET_NAME} indexes found in the table"
            ),
        });
    }
    // node_name and connected_to must be different
    for row in table.rows.iter().skip(1) {
        let node_name = row[NODE_NAME_IDX].trim();
        let connected_to = row
            .get(CONNECTED_TO_IDX)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        if let Some(peer) = connected_to
            && peer == node_name
        {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Step `{step}` error: {NODE_NAME} and {CONNECTED_TO} cannot be the same"
                ),
            });
        }
    }

    Ok(())
}

pub fn parse_wallet_resources_table_row(
    step: &str,
    row: &[String],
) -> Result<(String, WalletStartInfo, Option<String>), StepError> {
    let node_name = row[NODE_NAME_IDX].trim().to_owned();
    if node_name.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: {NODE_NAME} cannot be empty"),
        });
    }
    let account_index = row[ACCOUNT_INDEX_IDX_T2]
        .trim()
        .parse::<usize>()
        .map_err(|_| StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: {ACCOUNT_INDEX} '{}' must be a valid number",
                row[ACCOUNT_INDEX_IDX_T2]
            ),
        })?;
    let wallet_name = row[WALLET_NAME_IDX].trim().to_owned();
    if wallet_name.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: {WALLET_NAME} cannot be empty"),
        });
    }
    let connected_to = row
        .get(CONNECTED_TO_IDX)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Ok((
        node_name,
        WalletStartInfo {
            wallet_name,
            account_index,
        },
        connected_to,
    ))
}

pub fn verify_mining_node_wallet_resources_table_indexes(
    table: &Table,
    step: &str,
) -> Result<(), StepError> {
    if table.rows.is_empty()
        || table.rows[0].len() != 5
        || table.rows[0][NODE_NAME_IDX] != NODE_NAME
        || table.rows[0][ACCOUNT_INDEX_IDX_T2] != ACCOUNT_INDEX
        || table.rows[0][WALLET_NAME_IDX] != WALLET_NAME
        || table.rows[0][IS_MINING_WALLET_IDX] != IS_MINING_WALLET
        || table.rows[0][MINING_CONNECTED_TO_IDX] != CONNECTED_TO
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Mining wallet resources table must have a header row with columns: {NODE_NAME}, {ACCOUNT_INDEX}, {WALLET_NAME}, {IS_MINING_WALLET}, {CONNECTED_TO}"
            ),
        });
    }
    // All wallet indexes must be unique.
    let account_indexes: HashSet<_> = table
        .rows
        .iter()
        .map(|row| &row[ACCOUNT_INDEX_IDX_T2])
        .collect();
    if account_indexes.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {ACCOUNT_INDEX} indexes found in the table"
            ),
        });
    }
    // All wallet names must be unique.
    let wallet_names: HashSet<_> = table.rows.iter().map(|row| &row[WALLET_NAME_IDX]).collect();
    if wallet_names.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {WALLET_NAME} indexes found in the table"
            ),
        });
    }

    Ok(())
}

pub fn parse_mining_wallet_resources_table_row(
    step: &str,
    row: &[String],
) -> Result<(String, WalletStartInfo, bool, Option<String>), StepError> {
    let node_name = row[NODE_NAME_IDX].trim().to_owned();
    if node_name.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: {NODE_NAME} cannot be empty"),
        });
    }
    let account_index = row[ACCOUNT_INDEX_IDX_T2]
        .trim()
        .parse::<usize>()
        .map_err(|_| StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: {ACCOUNT_INDEX} '{}' must be a valid number",
                row[ACCOUNT_INDEX_IDX_T2]
            ),
        })?;
    let wallet_name = row[WALLET_NAME_IDX].trim().to_owned();
    if wallet_name.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: {WALLET_NAME} cannot be empty"),
        });
    }
    let is_mining_wallet = match row[IS_MINING_WALLET_IDX].trim() {
        "true" => true,
        "false" => false,
        other => {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Step `{step}` error: {IS_MINING_WALLET} '{other}' must be 'true' or 'false'"
                ),
            });
        }
    };
    let connected_to = row
        .get(MINING_CONNECTED_TO_IDX)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Ok((
        node_name,
        WalletStartInfo {
            wallet_name,
            account_index,
        },
        is_mining_wallet,
        connected_to,
    ))
}

pub fn ensure_fee_sponsorship_and_fork_groups_are_not_mixed(
    world: &CucumberWorld,
    step_value: &str,
) -> StepResult {
    if world
        .wallet_registry
        .fee_state
        .sponsored_genesis_account
        .is_some()
        && !world.fork_groups.groups().is_empty()
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step_value}` error: sponsored fee accounts cannot be combined with distinct node groups in the same scenario"
            ),
        });
    }

    Ok(())
}
