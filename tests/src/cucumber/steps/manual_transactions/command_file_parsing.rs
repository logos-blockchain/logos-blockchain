use std::{fs, path::Path};

use crate::cucumber::{error::StepError, steps::manual_transactions::utils::WalletStateType};

#[derive(Debug, Clone)]
pub enum ManualCommand {
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
