use super::{
    BTreeMap, CucumberWorld, Path, Serialize, StepError, StepResult, TARGET, Utxo, WalletError,
    WalletInfo, WalletOutputState, WalletStateView, WalletType,
    current_available_utxos_for_user_wallets, drain_node_wallet, drain_user_wallet, fs, info,
    utils, warn,
};

pub(super) async fn execute_drain(
    world: &mut CucumberWorld,
    step: &str,
    from: &str,
    to: &str,
) -> StepResult {
    let sender = world.resolve_wallet(from)?;
    let receiver_pk = world.resolve_recipient(to)?.public_key;

    if sender.public_key()? == receiver_pk {
        return Err(StepError::InvalidArgument {
            message: format!("Cannot drain wallet `{from}` into itself"),
        });
    }

    match sender.wallet_type {
        WalletType::User { .. } => drain_user_wallet(world, step, &sender, receiver_pk).await,
        WalletType::Funding { .. } => drain_node_wallet(world, &sender, receiver_pk).await,
    }
}

pub async fn log_wallet_balances(
    world: &mut CucumberWorld,
    step: &str,
    wallets: Vec<WalletInfo>,
) -> StepResult {
    let tracked_wallets = wallets
        .iter()
        .filter(|wallet| wallet.is_user_wallet())
        .cloned()
        .collect::<Vec<_>>();
    let states = if tracked_wallets.is_empty() {
        BTreeMap::new()
    } else {
        utils::current_wallet_states_for_wallets(world, step, &tracked_wallets).await?
    };

    for wallet in &wallets {
        if wallet.is_node_wallet() {
            log_node_wallet_balance(world, wallet).await?;
            continue;
        }
        let state =
            states
                .get(wallet.wallet_name.as_str())
                .ok_or_else(|| StepError::LogicalError {
                    message: format!(
                        "Wallet `{}` balance state is not tracked",
                        wallet.wallet_name
                    ),
                })?;
        log_wallet_state_balance(&wallet.wallet_name, &wallet.public_key_hex(), state);
    }
    Ok(())
}

async fn log_node_wallet_balance(world: &CucumberWorld, wallet: &WalletInfo) -> StepResult {
    let node = world
        .nodes_info
        .get(&wallet.node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!(
                "Node '{}' for wallet '{}' not found",
                wallet.node_name, wallet.wallet_name
            ),
        })?;

    let balance_response = node
        .started_node
        .client
        .wallet_balance(wallet.public_key()?, None)
        .await;
    match balance_response {
        Ok(balance) => {
            info!(
                target: TARGET,
                "Wallet `{}` [On-chain] {}/{} LGO, {}",
                wallet.wallet_name,
                balance.notes.len(),
                balance.balance,
                wallet.public_key_hex(),
            );
        }
        Err(_) => {
            info!(
                target: TARGET,
                "Wallet `{}` [On-chain] no funds yet, {}",
                wallet.wallet_name,
                wallet.public_key_hex(),
            );
        }
    }

    Ok(())
}

pub(super) async fn log_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> StepResult {
    let wallet = world.resolve_wallet(wallet_name)?;
    log_wallet_balances(world, step, vec![wallet]).await
}

fn log_wallet_state_balance(wallet_name: &str, public_key_hex: &str, state: &WalletStateView) {
    let available = state.balance(WalletOutputState::Available);
    let reserved = state.balance(WalletOutputState::Reserved);
    let on_chain = state.balance(WalletOutputState::OnChain);

    info!(
        target: TARGET,
        "Wallet `{wallet_name}` [Available] {}/{} LGO, [Encumbered] {}/{} LGO, \
        [On-chain] {}/{} LGO, {}",
        available.output_count,
        available.value,
        reserved.output_count,
        reserved.value,
        on_chain.output_count,
        on_chain.value,
        public_key_hex,
    );
}

#[derive(Serialize)]
struct WalletFundsExport {
    wallet: String,
    node_url: String,
    public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_key: Option<String>,
    requested_value: u64,
    selected_value: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u64>,
    utxos: Vec<ExportedUtxo>,
}

#[derive(Serialize)]
struct ExportedUtxo {
    utxo_id: String,
    value: u64,
    encoded_utxo: String,
}

pub(super) async fn export_funds(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    value: u64,
    output_path: &str,
    include_secret: bool,
) -> Result<(), StepError> {
    let wallet = world.resolve_wallet(wallet_name)?.clone();
    let available_utxos = current_available_utxos_for_user_wallets(world, step)
        .await?
        .get(wallet_name)
        .cloned()
        .ok_or(StepError::LogicalError {
            message: format!("Wallet '{wallet_name}' not found in updated balances"),
        })?;
    let selected = select_utxos_covering(available_utxos.clone(), value)?;
    let selected_value = selected.iter().map(|utxo| utxo.note.value).sum();
    let export = WalletFundsExport {
        wallet: wallet.wallet_name.clone(),
        node_url: format!(
            "{}",
            world
                .resolve_node_http_client(&wallet.node_name)?
                .base_url()
        ),
        public_key: wallet.public_key_hex(),
        secret_key: exported_secret_key(&wallet, include_secret)?,
        requested_value: value,
        selected_value,
        height: best_known_wallet_node_height(world, &wallet).await,
        utxos: selected
            .iter()
            .map(exported_utxo)
            .collect::<Result<Vec<_>, _>>()?,
    };

    let path = Path::new(output_path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| StepError::StepFail {
            message: format!(
                "Failed to create EXPORT_FUNDS output directory '{}': {error}",
                parent.display()
            ),
        })?;
    }
    let json = serde_json::to_string_pretty(&export).map_err(|error| StepError::StepFail {
        message: format!("Failed to serialize EXPORT_FUNDS JSON: {error}"),
    })?;
    fs::write(path, json).map_err(|error| StepError::StepFail {
        message: format!(
            "Failed to write EXPORT_FUNDS output '{}': {error}",
            path.display()
        ),
    })?;

    info!(
        target: TARGET,
        "EXPORT_FUNDS wrote {} UTXO(s), selected value {}, requested value {}, output '{}'",
        export.utxos.len(),
        selected_value,
        value,
        path.display()
    );
    Ok(())
}

fn select_utxos_covering(mut utxos: Vec<Utxo>, value: u64) -> Result<Vec<Utxo>, StepError> {
    utxos.sort_by_key(|utxo| std::cmp::Reverse(utxo.note.value));
    let available = utxos.iter().map(|utxo| utxo.note.value).sum();
    let mut selected = Vec::new();
    let mut selected_value = 0u64;

    for utxo in utxos {
        selected_value = selected_value.saturating_add(utxo.note.value);
        selected.push(utxo);
        if selected_value >= value {
            return Ok(selected);
        }
    }

    Err(StepError::WalletError(WalletError::InsufficientFunds {
        available,
    }))
}

fn exported_secret_key(
    wallet: &WalletInfo,
    include_secret: bool,
) -> Result<Option<String>, StepError> {
    if !include_secret {
        return Ok(None);
    }

    let WalletType::User { wallet_account } = &wallet.wallet_type else {
        return Err(StepError::InvalidArgument {
            message: format!(
                "EXPORT_FUNDS include_secret true requires a user wallet; '{}' is a funding wallet",
                wallet.wallet_name
            ),
        });
    };

    bincode::serialize(&wallet_account.secret_key)
        .map(hex::encode)
        .map(Some)
        .map_err(|error| StepError::StepFail {
            message: format!("Failed to encode wallet secret key for EXPORT_FUNDS: {error}"),
        })
}

fn exported_utxo(utxo: &Utxo) -> Result<ExportedUtxo, StepError> {
    Ok(ExportedUtxo {
        utxo_id: hex::encode(utxo.id().as_bytes()),
        value: utxo.note.value,
        encoded_utxo: bincode::serialize(utxo).map(hex::encode).map_err(|error| {
            StepError::StepFail {
                message: format!("Failed to encode UTXO for EXPORT_FUNDS: {error}"),
            }
        })?,
    })
}

async fn best_known_wallet_node_height(world: &CucumberWorld, wallet: &WalletInfo) -> Option<u64> {
    let node = world.nodes_info.get(&wallet.node_name)?;
    node.started_node
        .client
        .consensus_info()
        .await
        .ok()
        .map(|info| info.cryptarchia_info.height)
}

pub(super) fn clear_wallet_encumbrances(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> StepResult {
    if world.resolve_wallet(wallet_name).is_err() {
        warn!(target: TARGET, "Step `{}` error: wallet '{wallet_name}' not found in world state", step);
        return Err(StepError::LogicalError {
            message: format!("wallet '{wallet_name}' not found in world state"),
        });
    }

    world.with_wallets_mut(|wallets| wallets.clear_encumbrances(wallet_name))?;
    world
        .wallet_registry
        .fee_state
        .clear_wallet_reservations(wallet_name);
    info!(target: TARGET, "Cleared encumbrances for wallet '{wallet_name}'");
    Ok(())
}

pub(super) fn clear_all_wallet_encumbrances(world: &mut CucumberWorld, step: &str) -> StepResult {
    let wallet_names: Vec<String> = world.wallet_registry.wallet_info.keys().cloned().collect();

    for wallet_name in wallet_names {
        clear_wallet_encumbrances(world, step, &wallet_name)?;
    }
    info!(target: TARGET, "Cleared encumbrances for all wallets");
    Ok(())
}
