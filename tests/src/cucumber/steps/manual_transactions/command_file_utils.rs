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
    Send {
        transactions: usize,
        value: u64,
        from: String,
        to: String,
    },
    Continuous {
        coin_split_outputs: usize,
        coin_split_value: u64,
        transactions: usize,
        value: u64,
        cycles: usize,
    },
    Stop,
}

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

    for line in file_content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("---->") {
            updated_lines.push(line.to_owned());
            continue;
        }

        if selected.is_none() {
            selected = Some(parse_manual_command(trimmed)?);
            updated_lines.push(format!("----> {line}"));
            continue;
        }

        updated_lines.push(line.to_owned());
    }

    if selected.is_some() {
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
        "SEND" => Ok(ManualCommand::Send {
            transactions: parse_usize_field(&parts, "transactions")?,
            value: parse_u64_field(&parts, "value")?,
            from: parse_quoted_field(&parts, "from")?,
            to: parse_quoted_field(&parts, "to")?,
        }),
        "CONTINUOUS" => Ok(ManualCommand::Continuous {
            coin_split_outputs: parse_usize_field(&parts, "coin_split_outputs")?,
            coin_split_value: parse_u64_field(&parts, "coin_split_value")?,
            transactions: parse_usize_field(&parts, "transactions")?,
            value: parse_u64_field(&parts, "value")?,
            cycles: parse_usize_field(&parts, "cycles")?,
        }),
        "STOP" => Ok(ManualCommand::Stop),
        _ => Err(StepError::InvalidArgument {
            message: format!("Unknown manual command: '{action}'"),
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
