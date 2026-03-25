use std::{fs, path::Path};

use crate::cucumber::{error::StepError, steps::manual_transactions::utils::WalletStateType};

#[cfg_attr(test, derive(strum_macros::EnumCount))]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ManualCommand {
    CreateBlockchainSnapshotAllNodes {
        snapshot_name: String,
    },
    CreateBlockchainSnapshotNode {
        snapshot_name: String,
        node_name: String,
    },
    CoinSplit {
        wallet: String,
        outputs: usize,
        value: u64,
    },
    Verify {
        wallet: String,
        outputs: Option<usize>,
        value: Option<u64>,
        time_out: u64,
        wallet_state_type: WalletStateType,
        verify_max: bool,
    },
    WalletBalance {
        wallet_name: String,
    },
    WalletBalanceAllUserWallets,
    WalletBalanceAllFundingWallets,
    WalletBalanceAllWallets,
    ClearEncumbrances {
        wallet_name: String,
    },
    ClearEncumbrancesAllWallets,
    Send {
        transactions: usize,
        value: u64,
        from: String,
        to: String,
    },
    ContinuousUserWallets {
        coin_split_outputs: usize,
        coin_split_value: u64,
        transactions: usize,
        value: u64,
        cycles: usize,
    },
    ContinuousFundingWallets {
        coin_split_outputs: usize,
        coin_split_value: u64,
        transactions: usize,
        value: u64,
        cycles: usize,
    },
    FaucetFundsAllUserWallets {
        rounds: usize,
    },
    FaucetFundsAllFundingWallets {
        rounds: usize,
    },
    RestartNode {
        node_name: String,
    },
    CryptarchiaInfoAllNodes,
    WaitAllNodesSyncedToChain,
    Stop,
}

const PROCESSED_PREFIX: &str = "---->";
const ERROR_PREFIX: &str = "== ERROR == >";

pub(crate) fn take_next_command(path: &Path) -> Result<Option<ManualCommand>, StepError> {
    if !path.exists() {
        fs::write(path, "").map_err(|e| StepError::StepFail {
            message: format!(
                "Failed to initialize manual command file '{}': {e}",
                path.display()
            ),
        })?;
        return Ok(None);
    }

    let file_content = fs::read_to_string(path).map_err(|e| StepError::StepFail {
        message: format!(
            "Failed to read manual command file '{}': {e}",
            path.display()
        ),
    })?;

    let mut updated_lines = Vec::new();
    let mut selected = None;
    let mut file_changed = false;

    for line in file_content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with(PROCESSED_PREFIX)
            || trimmed.starts_with(ERROR_PREFIX)
        {
            updated_lines.push(line.to_owned());
            continue;
        }

        if selected.is_none() {
            match parse_manual_command(trimmed) {
                Ok(command) => {
                    selected = Some(command);
                    updated_lines.push(format!("{PROCESSED_PREFIX} {line}"));
                    file_changed = true;
                }
                Err(error) => {
                    tracing::warn!(
                        "Ignoring invalid manual command in '{}': {} (line: '{}')",
                        path.display(),
                        error,
                        trimmed
                    );
                    updated_lines.push(format!("{ERROR_PREFIX} {line}"));
                    file_changed = true;
                }
            }
            continue;
        }

        updated_lines.push(line.to_owned());
    }

    if file_changed {
        fs::write(path, updated_lines.join("\n")).map_err(|e| StepError::StepFail {
            message: format!(
                "Failed to update manual command file '{}' after processing command: {e}",
                path.display()
            ),
        })?;
    }

    Ok(selected)
}

#[expect(
    clippy::too_many_lines,
    reason = "Enum match arms - useful to have in a single place."
)]
fn parse_manual_command(raw: &str) -> Result<ManualCommand, StepError> {
    let parts: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let Some(action) = parts.first() else {
        return Err(StepError::InvalidArgument {
            message: "Manual command is empty".to_owned(),
        });
    };

    let binding = action.to_ascii_uppercase();
    let command = binding.as_str();
    match command {
        "CREATE_BLOCKCHAIN_SNAPSHOT_ALL_NODES" => {
            Ok(ManualCommand::CreateBlockchainSnapshotAllNodes {
                snapshot_name: parse_quoted_field(&parts, "snapshot_name")?,
            })
        }
        "CREATE_BLOCKCHAIN_SNAPSHOT_NODE" => Ok(ManualCommand::CreateBlockchainSnapshotNode {
            snapshot_name: parse_quoted_field(&parts, "snapshot_name")?,
            node_name: parse_quoted_field(&parts, "node_name")?,
        }),
        "COIN_SPLIT" => Ok(ManualCommand::CoinSplit {
            wallet: parse_quoted_field(&parts, "wallet")?,
            outputs: parse_usize_field(&parts, "outputs")?,
            value: parse_u64_field(&parts, "value")?,
        }),
        "VERIFY_MAX" | "VERIFY_MIN" => {
            let outputs = parse_optional_usize_field(&parts, "outputs")?;
            let value = parse_optional_u64_field(&parts, "value")?;
            if outputs.is_none() && value.is_none() {
                return Err(StepError::InvalidArgument {
                    message: format!(
                        "{command} command requires at least one of 'outputs' or 'value'"
                    ),
                });
            }
            let wallet = parse_quoted_field(&parts, "wallet")?;
            let time_out = parse_u64_field(&parts, "time_out")?;
            let wallet_state_type =
                parse_quoted_field(&parts, "wallet_state_type").and_then(|s| {
                    s.parse::<WalletStateType>()
                        .map_err(|e| StepError::InvalidArgument {
                            message: format!("Invalid 'wallet_state_type' value: {e}"),
                        })
                })?;
            Ok(ManualCommand::Verify {
                wallet,
                outputs,
                value,
                time_out,
                wallet_state_type,
                verify_max: command == "VERIFY_MAX",
            })
        }
        "BALANCE" => Ok(ManualCommand::WalletBalance {
            wallet_name: parse_quoted_field(&parts, "wallet")?,
        }),
        "BALANCE_ALL_USER_WALLETS" => Ok(ManualCommand::WalletBalanceAllUserWallets),
        "BALANCE_ALL_FUNDING_WALLETS" => Ok(ManualCommand::WalletBalanceAllFundingWallets),
        "BALANCE_ALL_WALLETS" => Ok(ManualCommand::WalletBalanceAllWallets),
        "CLEAR_ENCUMBRANCES" => Ok(ManualCommand::ClearEncumbrances {
            wallet_name: parse_quoted_field(&parts, "wallet")?,
        }),
        "CLEAR_ENCUMBRANCES_ALL_WALLETS" => Ok(ManualCommand::ClearEncumbrancesAllWallets),
        "SEND" => Ok(ManualCommand::Send {
            transactions: parse_usize_field(&parts, "transactions")?,
            value: parse_u64_field(&parts, "value")?,
            from: parse_quoted_field(&parts, "from")?,
            to: parse_quoted_field(&parts, "to")?,
        }),
        "CONTINUOUS_USER_WALLETS" => Ok(ManualCommand::ContinuousUserWallets {
            coin_split_outputs: parse_usize_field(&parts, "coin_split_outputs")?,
            coin_split_value: parse_u64_field(&parts, "coin_split_value")?,
            transactions: parse_usize_field(&parts, "transactions")?,
            value: parse_u64_field(&parts, "value")?,
            cycles: parse_usize_field(&parts, "cycles")?,
        }),
        "CONTINUOUS_FUNDING_WALLETS" => Ok(ManualCommand::ContinuousFundingWallets {
            coin_split_outputs: parse_usize_field(&parts, "coin_split_outputs")?,
            coin_split_value: parse_u64_field(&parts, "coin_split_value")?,
            transactions: parse_usize_field(&parts, "transactions")?,
            value: parse_u64_field(&parts, "value")?,
            cycles: parse_usize_field(&parts, "cycles")?,
        }),
        "FAUCET_ALL_USER_WALLETS" => Ok(ManualCommand::FaucetFundsAllUserWallets {
            rounds: parse_usize_field(&parts, "rounds")?,
        }),
        "FAUCET_ALL_FUNDING_WALLETS" => Ok(ManualCommand::FaucetFundsAllFundingWallets {
            rounds: parse_usize_field(&parts, "rounds")?,
        }),
        "RESTART_NODE" => Ok(ManualCommand::RestartNode {
            node_name: parse_quoted_field(&parts, "node_name")?,
        }),
        "CRYPTARCHIA_INFO_ALL_NODES" => Ok(ManualCommand::CryptarchiaInfoAllNodes),
        "WAIT_ALL_NODES_SYNCED_TO_CHAIN" => Ok(ManualCommand::WaitAllNodesSyncedToChain),
        "STOP" => Ok(ManualCommand::Stop),
        _ => Err(StepError::InvalidArgument {
            message: format!("Unknown manual command: '{action}' in '{raw}'"),
        }),
    }
}

fn parse_quoted_field(parts: &[String], key: &str) -> Result<String, StepError> {
    parts
        .iter()
        .find_map(|part| {
            let normalized = part.trim();
            normalized
                .strip_prefix(&format!("{key} '"))
                .and_then(|v| v.strip_suffix('\''))
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| StepError::InvalidArgument {
            message: format!("Missing required field '{key}'"),
        })
}

fn parse_u64_field(parts: &[String], key: &str) -> Result<u64, StepError> {
    let raw = parse_number_field(parts, key)?;
    raw.parse::<u64>().map_err(|_| StepError::InvalidArgument {
        message: format!("Invalid value for '{key}': '{raw}'"),
    })
}

fn parse_optional_u64_field(parts: &[String], key: &str) -> Result<Option<u64>, StepError> {
    let raw = parse_optional_number_field(parts, key);
    raw.map_or(Ok(None), |raw: &str| {
        raw.parse::<u64>()
            .map(Some)
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Invalid value for '{key}': '{raw}'"),
            })
    })
}

fn parse_usize_field(parts: &[String], key: &str) -> Result<usize, StepError> {
    let raw = parse_number_field(parts, key)?;
    raw.parse::<usize>()
        .map_err(|_| StepError::InvalidArgument {
            message: format!("Invalid value for '{key}': '{raw}'"),
        })
}

fn parse_optional_usize_field(parts: &[String], key: &str) -> Result<Option<usize>, StepError> {
    let raw = parse_optional_number_field(parts, key);
    raw.map_or(Ok(None), |raw: &str| {
        raw.parse::<usize>()
            .map(Some)
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Invalid value for '{key}': '{raw}'"),
            })
    })
}

fn parse_number_field<'a>(parts: &'a [String], key: &str) -> Result<&'a str, StepError> {
    parse_optional_number_field(parts, key).ok_or_else(|| StepError::InvalidArgument {
        message: format!("Missing required field '{key}'"),
    })
}

fn parse_optional_number_field<'a>(parts: &'a [String], key: &str) -> Option<&'a str> {
    for part in parts {
        let normalized = part.trim();
        if let Some(value) = normalized.strip_prefix(&format!("{key} ")) {
            return Some(value.trim());
        }
        if let Some(value) = normalized.strip_prefix(&format!("{key}=")) {
            return Some(value.trim());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use strum::EnumCount as _;

    use super::{ManualCommand, WalletStateType, parse_manual_command};

    fn parse_ok(raw: &str) -> ManualCommand {
        parse_manual_command(raw)
            .unwrap_or_else(|e| panic!("Expected command to parse, got error: {e}. Raw: {raw}"))
    }

    fn assert_create_blockchain_snapshot_all_nodes_command() {
        let command =
            parse_ok("CREATE_BLOCKCHAIN_SNAPSHOT_ALL_NODES, snapshot_name 'SNAP_TEST_01'");

        assert!(matches!(
            command,
            ManualCommand::CreateBlockchainSnapshotAllNodes { snapshot_name }
                if snapshot_name == "SNAP_TEST_01"
        ));
    }

    fn assert_create_blockchain_snapshot_node_command() {
        let command = parse_ok(
            "CREATE_BLOCKCHAIN_SNAPSHOT_NODE, snapshot_name 'SNAP_TEST_01', node_name 'NODE_1'",
        );

        assert!(matches!(
            command,
            ManualCommand::CreateBlockchainSnapshotNode {
                snapshot_name,
                node_name,
            } if snapshot_name == "SNAP_TEST_01" && node_name == "NODE_1"
        ));
    }

    fn assert_coin_split_command() {
        let command = parse_ok("COIN_SPLIT, wallet 'WALLET_1A', outputs 10, value 100");

        assert!(matches!(
            command,
            ManualCommand::CoinSplit {
                wallet,
                outputs,
                value,
            } if wallet == "WALLET_1A" && outputs == 10 && value == 100
        ));
    }

    fn assert_verify_max_command() {
        let command = parse_ok(
            "VERIFY_MAX, wallet 'WALLET_1A', wallet_state_type 'encumbered', outputs 0, value 14000, time_out 60",
        );

        assert!(matches!(
            command,
            ManualCommand::Verify {
                wallet,
                outputs,
                value,
                time_out,
                wallet_state_type: WalletStateType::Encumbered,
                verify_max,
            } if wallet == "WALLET_1A"
                && outputs == Some(0)
                && value == Some(14000)
                && time_out == 60
                && verify_max
        ));
    }

    fn assert_verify_min_command() {
        let command = parse_ok(
            "VERIFY_MIN, wallet 'WALLET_2A', wallet_state_type 'on-chain', outputs 1, value 10, time_out 30",
        );

        assert!(matches!(
            command,
            ManualCommand::Verify {
                wallet,
                outputs,
                value,
                time_out,
                wallet_state_type: WalletStateType::OnChain,
                verify_max,
            } if wallet == "WALLET_2A"
                && outputs == Some(1)
                && value == Some(10)
                && time_out == 30
                && !verify_max
        ));
    }

    fn assert_balance_command() {
        let command = parse_ok("BALANCE, wallet 'WALLET_1A'");

        assert!(matches!(
            command,
            ManualCommand::WalletBalance { wallet_name } if wallet_name == "WALLET_1A"
        ));
    }

    fn assert_balance_all_user_wallets_command() {
        let command = parse_ok("BALANCE_ALL_USER_WALLETS");
        assert!(matches!(
            command,
            ManualCommand::WalletBalanceAllUserWallets
        ));
    }

    fn assert_balance_all_funding_wallets_command() {
        let command = parse_ok("BALANCE_ALL_FUNDING_WALLETS");
        assert!(matches!(
            command,
            ManualCommand::WalletBalanceAllFundingWallets
        ));
    }

    fn assert_balance_all_wallets_command() {
        let command = parse_ok("BALANCE_ALL_WALLETS");
        assert!(matches!(command, ManualCommand::WalletBalanceAllWallets));
    }

    fn assert_clear_encumbrances_command() {
        let command = parse_ok("CLEAR_ENCUMBRANCES, wallet 'WALLET_2A'");

        assert!(matches!(
            command,
            ManualCommand::ClearEncumbrances { wallet_name } if wallet_name == "WALLET_2A"
        ));
    }

    fn assert_clear_encumbrances_all_wallets_command() {
        let command = parse_ok("CLEAR_ENCUMBRANCES_ALL_WALLETS");
        assert!(matches!(
            command,
            ManualCommand::ClearEncumbrancesAllWallets
        ));
    }

    fn assert_send_command() {
        let command = parse_ok("SEND, transactions 5, value 100, from 'WALLET_1A', to 'WALLET_2A'");

        assert!(matches!(
            command,
            ManualCommand::Send {
                transactions,
                value,
                from,
                to,
            } if transactions == 5 && value == 100 && from == "WALLET_1A" && to == "WALLET_2A"
        ));
    }

    fn assert_continuous_user_wallets_command() {
        let command = parse_ok(
            "CONTINUOUS_USER_WALLETS, coin_split_outputs 10, coin_split_value 100, transactions 4, value 50, cycles 3",
        );

        assert!(matches!(
            command,
            ManualCommand::ContinuousUserWallets {
                coin_split_outputs,
                coin_split_value,
                transactions,
                value,
                cycles,
            } if coin_split_outputs == 10
                && coin_split_value == 100
                && transactions == 4
                && value == 50
                && cycles == 3
        ));
    }

    fn assert_continuous_funding_wallets_command() {
        let command = parse_ok(
            "CONTINUOUS_FUNDING_WALLETS, coin_split_outputs 8, coin_split_value 25, transactions 3, value 20, cycles 2",
        );

        assert!(matches!(
            command,
            ManualCommand::ContinuousFundingWallets {
                coin_split_outputs,
                coin_split_value,
                transactions,
                value,
                cycles,
            } if coin_split_outputs == 8
                && coin_split_value == 25
                && transactions == 3
                && value == 20
                && cycles == 2
        ));
    }

    fn assert_faucet_all_user_wallets_command() {
        let command = parse_ok("FAUCET_ALL_USER_WALLETS, rounds 3");

        assert!(matches!(
            command,
            ManualCommand::FaucetFundsAllUserWallets { rounds } if rounds == 3
        ));
    }

    fn assert_faucet_all_funding_wallets_command() {
        let command = parse_ok("FAUCET_ALL_FUNDING_WALLETS, rounds 2");

        assert!(matches!(
            command,
            ManualCommand::FaucetFundsAllFundingWallets { rounds } if rounds == 2
        ));
    }

    fn assert_cryptarchia_info_all_nodes_command() {
        let command = parse_ok("CRYPTARCHIA_INFO_ALL_NODES");
        assert!(matches!(command, ManualCommand::CryptarchiaInfoAllNodes));
    }

    fn assert_restart_node_command() {
        let command = parse_ok("RESTART_NODE, node_name 'NODE_01'");
        assert!(matches!(
            command,
            ManualCommand::RestartNode { node_name } if node_name == "NODE_01"
        ));
    }

    fn assert_wait_all_nodes_synced_to_chain_command() {
        let command = parse_ok("WAIT_ALL_NODES_SYNCED_TO_CHAIN");
        assert!(matches!(command, ManualCommand::WaitAllNodesSyncedToChain));
    }

    fn assert_stop_command() {
        let command = parse_ok("STOP");
        assert!(matches!(command, ManualCommand::Stop));
    }

    fn variant_array() -> [ManualCommand; ManualCommand::COUNT] {
        let command_array = [
            ManualCommand::CreateBlockchainSnapshotAllNodes {
                snapshot_name: String::new(),
            },
            ManualCommand::CreateBlockchainSnapshotNode {
                snapshot_name: String::new(),
                node_name: String::new(),
            },
            ManualCommand::CoinSplit {
                wallet: String::new(),
                outputs: 0,
                value: 0,
            },
            ManualCommand::Verify {
                wallet: String::new(),
                outputs: None,
                value: None,
                time_out: 0,
                wallet_state_type: WalletStateType::OnChain,
                verify_max: false,
            },
            ManualCommand::WalletBalance {
                wallet_name: String::new(),
            },
            ManualCommand::WalletBalanceAllUserWallets,
            ManualCommand::WalletBalanceAllFundingWallets,
            ManualCommand::WalletBalanceAllWallets,
            ManualCommand::ClearEncumbrances {
                wallet_name: String::new(),
            },
            ManualCommand::ClearEncumbrancesAllWallets,
            ManualCommand::Send {
                transactions: 0,
                value: 0,
                from: String::new(),
                to: String::new(),
            },
            ManualCommand::ContinuousUserWallets {
                coin_split_outputs: 0,
                coin_split_value: 0,
                transactions: 0,
                value: 0,
                cycles: 0,
            },
            ManualCommand::ContinuousFundingWallets {
                coin_split_outputs: 0,
                coin_split_value: 0,
                transactions: 0,
                value: 0,
                cycles: 0,
            },
            ManualCommand::FaucetFundsAllUserWallets { rounds: 0 },
            ManualCommand::FaucetFundsAllFundingWallets { rounds: 0 },
            ManualCommand::RestartNode {
                node_name: String::new(),
            },
            ManualCommand::CryptarchiaInfoAllNodes,
            ManualCommand::WaitAllNodesSyncedToChain,
            ManualCommand::Stop,
        ];
        let mut test_array = command_array
            .iter()
            .map(|c| format!("{c:?}"))
            .collect::<Vec<_>>();
        test_array.sort_by_key(|c| format!("{c:?}"));
        test_array.dedup();
        assert_eq!(
            test_array.len(),
            ManualCommand::COUNT,
            "All ManualCommand variants must be unique"
        );
        command_array
    }

    #[test]
    fn manual_command_parse_test_covers_all_variants() {
        let mut visited = 0;

        for variant in variant_array() {
            match variant {
                ManualCommand::CreateBlockchainSnapshotAllNodes { .. } => {
                    assert_create_blockchain_snapshot_all_nodes_command();
                    visited += 1;
                }
                ManualCommand::CreateBlockchainSnapshotNode { .. } => {
                    assert_create_blockchain_snapshot_node_command();
                    visited += 1;
                }
                ManualCommand::CoinSplit { .. } => {
                    assert_coin_split_command();
                    visited += 1;
                }
                ManualCommand::Verify { .. } => {
                    assert_verify_max_command();
                    assert_verify_min_command();
                    visited += 1;
                }
                ManualCommand::WalletBalance { .. } => {
                    assert_balance_command();
                    visited += 1;
                }
                ManualCommand::WalletBalanceAllUserWallets => {
                    assert_balance_all_user_wallets_command();
                    visited += 1;
                }
                ManualCommand::WalletBalanceAllFundingWallets => {
                    assert_balance_all_funding_wallets_command();
                    visited += 1;
                }
                ManualCommand::WalletBalanceAllWallets => {
                    assert_balance_all_wallets_command();
                    visited += 1;
                }
                ManualCommand::ClearEncumbrances { .. } => {
                    assert_clear_encumbrances_command();
                    visited += 1;
                }
                ManualCommand::ClearEncumbrancesAllWallets => {
                    assert_clear_encumbrances_all_wallets_command();
                    visited += 1;
                }
                ManualCommand::Send { .. } => {
                    assert_send_command();
                    visited += 1;
                }
                ManualCommand::ContinuousUserWallets { .. } => {
                    assert_continuous_user_wallets_command();
                    visited += 1;
                }
                ManualCommand::ContinuousFundingWallets { .. } => {
                    assert_continuous_funding_wallets_command();
                    visited += 1;
                }
                ManualCommand::FaucetFundsAllUserWallets { .. } => {
                    assert_faucet_all_user_wallets_command();
                    visited += 1;
                }
                ManualCommand::FaucetFundsAllFundingWallets { .. } => {
                    assert_faucet_all_funding_wallets_command();
                    visited += 1;
                }
                ManualCommand::RestartNode { .. } => {
                    assert_restart_node_command();
                    visited += 1;
                }
                ManualCommand::CryptarchiaInfoAllNodes => {
                    assert_cryptarchia_info_all_nodes_command();
                    visited += 1;
                }
                ManualCommand::WaitAllNodesSyncedToChain => {
                    assert_wait_all_nodes_synced_to_chain_command();
                    visited += 1;
                }
                ManualCommand::Stop => {
                    assert_stop_command();
                    visited += 1;
                }
            }
        }

        assert_eq!(visited, ManualCommand::COUNT);
    }
}
