use std::{collections::HashMap, fmt::Display, num::NonZero, time::Duration};

use hex::ToHex as _;
use lb_core::{
    codec::SerializeOp as _,
    mantle::{
        Note, NoteId, SignedMantleTx, Transaction as _, Utxo, gas::MainnetGasConstants,
        tx_builder::MantleTxBuilder,
    },
};
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
    world::{CucumberWorld, WalletInfo, WalletTokenMap},
};

/// Specifies which subset of wallet UTXOs to consider when checking for
/// expected wallet state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WalletStateType {
    /// All UTXOs that are on-chain, regardless of whether they are encumbered
    /// or not.
    OnChain,
    /// UTXOs that are currently encumbered by pending transactions.
    Encumbered,
    /// UTXOs that are not encumbered and are available for new transactions.
    Available,
}

impl Display for WalletStateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OnChain => write!(f, "on-chain"),
            Self::Encumbered => write!(f, "encumbered"),
            Self::Available => write!(f, "available"),
        }
    }
}

use std::str::FromStr;

use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;

use crate::cucumber::{
    defaults::CUCUMBER_VERBOSE_CONSOLE, steps::manual_transactions::faucet::FaucetTask,
    utils::is_truthy_env, world::WalletType,
};

impl FromStr for WalletStateType {
    type Err = StepError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on-chain" => Ok(Self::OnChain),
            "encumbered" => Ok(Self::Encumbered),
            "available" => Ok(Self::Available),
            _ => Err(StepError::InvalidArgument {
                message: format!("Unknown WalletStateType: '{s}'"),
            }),
        }
    }
}

pub async fn create_and_submit_transaction(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    receivers: &[(ZkPublicKey, u64)],
) -> Result<String, StepError> {
    let wallet = world.resolve_wallet(sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let available_utxos = update_wallet_balance(world, step, sender_wallet_name).await?;

    let tx_hashes = match wallet.wallet_type {
        WalletType::User {
            ref wallet_account, ..
        } => {
            let wallet_state = wallet_state_from_utxos(available_utxos);
            let mut tx_builder = MantleTxBuilder::new();
            for (receiver_pk, value) in receivers {
                tx_builder = tx_builder.add_ledger_output(Note::new(*value, *receiver_pk));
            }

            let sender_pk = wallet_account.public_key();
            let funded_builder = wallet_state
                .fund_tx::<MainnetGasConstants>(&tx_builder, sender_pk, [sender_pk])
                .inspect_err(|e| {
                    warn!(target: TARGET, "Step `{}` error: {e}", step);
                })?;
            // Collect all UTXOs used in this transaction as encumbered tokens to prevent
            // them from being used in other transactions until this transaction is
            // finalized.
            let newly_encumbered: Vec<Utxo> = funded_builder.ledger_inputs().to_vec();

            let mantle_tx = funded_builder.build();
            let tx_hash = mantle_tx.hash();
            let ledger_tx_proof = ZkKey::multi_sign(
                std::slice::from_ref(&wallet_account.secret_key),
                tx_hash.as_ref(),
            )
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?;

            let signed_tx =
                SignedMantleTx::new(mantle_tx, vec![], ledger_tx_proof).inspect_err(|e| {
                    warn!(target: TARGET, "Step `{}` error: {e}", step);
                })?;

            world
                .submit_transaction(&wallet, &signed_tx)
                .await
                .inspect_err(|e| {
                    warn!(target: TARGET, "Step `{}` error: {e}", step);
                })?;
            world
                .wallet_encumbered_tokens
                .entry(sender_wallet_name.to_owned())
                .or_default()
                .extend(newly_encumbered);

            vec![tx_hash]
        }
        WalletType::Funding { .. } => {
            let mut tx_hashes = Vec::with_capacity(receivers.len());
            for (receiver_pk, value) in receivers {
                let body = WalletTransferFundsRequestBody {
                    tip: None,
                    change_public_key: wallet.public_key()?,
                    funding_public_keys: vec![wallet.public_key()?],
                    recipient_public_key: *receiver_pk,
                    amount: *value,
                };
                let tx_hash = world
                    .submit_funding_wallet_transaction(&wallet, body)
                    .await
                    .inspect_err(|e| {
                        warn!(target: TARGET, "Step `{}` error: {e}", step);
                    })?;
                tx_hashes.push(tx_hash);
            }
            tx_hashes
        }
    };

    let tx_hashes_hex: String = tx_hashes
        .iter()
        .map(|h| {
            h.to_bytes()
                .unwrap()
                .to_ascii_lowercase()
                .encode_hex::<String>()
        })
        .collect::<Vec<_>>()
        .join(", ");

    Ok(tx_hashes_hex)
}

pub async fn update_wallet_balance_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<(), StepError> {
    let wallets = world.all_user_wallets();
    for wallet in wallets {
        let _unused = update_wallet_balance(world, step, &wallet.wallet_name).await;
    }

    Ok(())
}

pub async fn update_wallet_balance_all_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<(), StepError> {
    let wallets = world.all_funding_wallets();
    for wallet in wallets {
        let _unused = update_wallet_balance(world, step, &wallet.wallet_name).await;
    }

    Ok(())
}

pub async fn update_wallet_balance_all_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<(), StepError> {
    let mut all_wallets = world.all_user_wallets();
    let mut funding_wallets = world.all_funding_wallets();
    all_wallets.append(&mut funding_wallets);
    for wallet in &all_wallets {
        let _unused = update_wallet_balance(world, step, &wallet.wallet_name).await;
    }

    Ok(())
}
pub async fn update_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
) -> Result<Vec<Utxo>, StepError> {
    let wallet = world.resolve_wallet(sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let sender_pk = wallet.public_key()?;
    let on_chain_utxos =
        collect_wallet_utxos(world, sender_wallet_name, &wallet.node_name, sender_pk)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?;

    let (on_chain_utxos_len, on_chain_utxos_sum) = (
        on_chain_utxos.len(),
        on_chain_utxos
            .iter()
            .map(|u| u.note.value)
            .collect::<Vec<_>>()
            .iter()
            .sum::<u64>(),
    );

    let mut available_utxos = on_chain_utxos;
    let encumbered_utxos = world
        .wallet_encumbered_tokens
        .get(sender_wallet_name)
        .map_or_else(Vec::new, |encumbered| {
            available_utxos.retain(|utxo| !encumbered.contains(utxo));
            encumbered.clone()
        });

    info!(
        target: TARGET,
        "Wallet `{sender_wallet_name}` [Available] {}/{} LGO, [Encumbered] {}/{} LGO, [On-chain] \
        {on_chain_utxos_len}/{on_chain_utxos_sum} LGO",
        available_utxos.len(),
        available_utxos.iter().map(|u| u.note.value).collect::<Vec<_>>().iter().sum::<u64>(),
        encumbered_utxos.len(),
        encumbered_utxos.iter().map(|u| u.note.value).collect::<Vec<_>>().iter().sum::<u64>(),
    );

    Ok(available_utxos)
}

async fn get_output_balances(
    world: &mut CucumberWorld,
    step: &str,
    wallet: &WalletInfo,
    wallet_name: &str,
    wallet_state_type: WalletStateType,
) -> Result<(usize, u64), StepError> {
    let on_chain_utxos =
        collect_wallet_utxos(world, wallet_name, &wallet.node_name, wallet.public_key()?)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?;

    match wallet_state_type {
        WalletStateType::OnChain => Ok((
            on_chain_utxos.len(),
            on_chain_utxos.iter().map(|u| u.note.value).sum::<u64>(),
        )),
        WalletStateType::Encumbered => {
            let encumbered_utxos = world
                .wallet_encumbered_tokens
                .get(wallet_name)
                .cloned()
                .unwrap_or_default();
            Ok((
                encumbered_utxos.len(),
                encumbered_utxos.iter().map(|u| u.note.value).sum::<u64>(),
            ))
        }
        WalletStateType::Available => {
            let available_utxos: Vec<_> = on_chain_utxos
                .iter()
                .filter(|utxo| {
                    !world
                        .wallet_encumbered_tokens
                        .get(wallet_name)
                        .is_some_and(|enc| enc.contains(utxo))
                })
                .collect();
            Ok((
                available_utxos.len(),
                available_utxos.iter().map(|u| u.note.value).sum::<u64>(),
            ))
        }
    }
}

pub async fn get_wallet_balances(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    wallet_state_type: WalletStateType,
) -> Result<(usize, u64), StepError> {
    let wallet = world.resolve_wallet(wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    get_output_balances(world, step, &wallet, wallet_name, wallet_state_type).await
}

pub fn clear_wallet_encumbrances(
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

    world.wallet_encumbered_tokens.remove(wallet_name);
    info!(target: TARGET, "Cleared encumbrances for wallet '{wallet_name}'");
    Ok(())
}

pub fn clear_all_wallet_encumbrances(world: &mut CucumberWorld, step: &str) -> StepResult {
    let wallet_names: Vec<String> = world.wallet_info.keys().cloned().collect();

    for wallet_name in wallet_names {
        clear_wallet_encumbrances(world, step, &wallet_name)?;
    }
    info!(target: TARGET, "Cleared encumbrances for all wallets");
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "This function is more readable with explicit arguments rather than packing them into structs or tuples."
)]
pub async fn wait_for_wallet_or_encumbered_state(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: String,
    min_coin_count: Option<&usize>,
    max_coin_count: Option<&usize>,
    min_token_value: Option<&u64>,
    max_token_value: Option<&u64>,
    time_out_seconds: u64,
    wallet_state_type: WalletStateType,
) -> StepResult {
    if min_coin_count.is_none()
        && min_token_value.is_none()
        && max_coin_count.is_none()
        && max_token_value.is_none()
    {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{step}` error: at least one of 'min_coin_count', 'max_coin_count', \
                'min_token_value' or 'max_token_value' must be provided"
            ),
        });
    }
    if min_coin_count.is_some() && max_coin_count.is_some() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{step}` error: 'min_coin_count' and 'max_coin_count' cannot be used \
                together"
            ),
        });
    }
    if min_token_value.is_some() && max_token_value.is_some() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{step}` error: 'min_token_value' and 'max_token_value' cannot be used \
                together"
            ),
        });
    }

    let wallet = world
        .wallet_info
        .get(&wallet_name)
        .ok_or(StepError::LogicalError {
            message: format!("wallet '{wallet_name}' not found in world state"),
        })?
        .clone();

    let start = Instant::now();
    let time_out = Duration::from_secs(time_out_seconds);
    let mut poll_count = 0usize;

    loop {
        let (coin_count, value) =
            get_output_balances(world, step, &wallet, &wallet_name, wallet_state_type).await?;

        if conditions_met(
            &wallet_name,
            &wallet.node_name,
            coin_count,
            value,
            min_coin_count,
            max_coin_count,
            min_token_value,
            max_token_value,
            wallet_state_type,
        ) {
            return Ok(());
        }

        if start.elapsed() >= time_out {
            let msg = format!(
                "Step `{step}` error: wallet '{wallet_name}/{}' has `outputs/LGO` = `{coin_count}/ \
                {value}`, expected `outputs/LGO` >= `{min_coin_count:?}/{min_token_value:?}` \
                or `outputs/LGO` <= `{max_coin_count:?}/{max_token_value:?}`",
                wallet.node_name,
            );
            warn!(target: TARGET, "{msg}");
            return Err(StepError::StepFail { message: msg });
        }

        if poll_count.is_multiple_of(25) {
            info!(
                target: TARGET,
                "Waiting for wallet '{wallet_name}/{}' to have required '{wallet_state_type}' \
                coins, current count: {coin_count}, value: {value}",
                wallet.node_name
            );
        }
        poll_count += 1;
        sleep(Duration::from_millis(200)).await;
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "This function is more readable with explicit arguments rather than packing them into structs or tuples."
)]
fn conditions_met(
    wallet_name: &str,
    wallet_node_name: &str,
    coin_count: usize,
    value: u64,
    min_coin_count: Option<&usize>,
    max_coin_count: Option<&usize>,
    min_token_value: Option<&u64>,
    max_token_value: Option<&u64>,
    wallet_state_type: WalletStateType,
) -> bool {
    match (
        min_coin_count,
        min_token_value,
        max_coin_count,
        max_token_value,
    ) {
        (Some(min_count), Some(min_value), _, _) => {
            if coin_count >= *min_count && value >= *min_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} >= \
                    {min_count}, token value: {value} >= {min_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (Some(min_count), None, _, _) => {
            if coin_count >= *min_count {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} >= \
                    {min_count}",
                    wallet_node_name
                );
                return true;
            }
        }
        (None, Some(min_value), _, _) => {
            if value >= *min_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' token value: {value} >= \
                    {min_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (_, _, Some(max_count), Some(max_value)) => {
            if coin_count <= *max_count && value <= *max_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} <= \
                    {max_count}, token value: {value} <= {max_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (_, _, Some(max_count), None) => {
            if coin_count <= *max_count {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} <= \
                    {max_count}",
                    wallet_node_name
                );
                return true;
            }
        }
        (_, _, None, Some(max_value)) => {
            if value <= *max_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' token value: {value} <= \
                    {max_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (None, None, None, None) => unreachable!(),
    }

    false
}

fn record_header_height(
    node_header_heights: &mut HashMap<String, HashMap<String, u64>>,
    node_name: &str,
    header_id: &str,
    height: u64,
) {
    node_header_heights
        .entry(node_name.to_owned())
        .or_default()
        .insert(header_id.to_owned(), height);
}

fn get_last_known_height<'a>(
    node_header_heights: &HashMap<String, HashMap<String, u64>>,
    cached_ancestor_header_id: Option<&String>,
    wallet_node_name: &'a str,
    reached_chain_start: bool,
) -> (u64, &'a str) {
    // Height of the first block in tail_blocks (oldest uncached block)
    cached_ancestor_header_id.as_ref().map_or(
        (1, if reached_chain_start { "" } else { "~" }),
        |&cached_header_id| {
            let cached_height = node_header_heights
                .get(wallet_node_name)
                .and_then(|m| m.get(cached_header_id))
                .copied();

            cached_height.map_or((1, "~"), |h| (h + 1, ""))
        },
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "This function is necessarily complex due to the logic of reconstructing wallet state from blocks and caching it for performance."
)]
async fn collect_wallet_utxos(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_node_name: &str,
    wallet_pk: ZkPublicKey,
) -> Result<Vec<Utxo>, StepError> {
    let mut owned: HashMap<NoteId, Utxo> = HashMap::new();

    // Add genesis block UTXOs to the owned set, as they are not included in the
    // blocks stream.
    for utxo in &world.genesis_block_utxos {
        if utxo.note.pk == wallet_pk {
            owned.insert(utxo.id(), *utxo);
        }
    }

    let node = world
        .nodes_info
        .get(wallet_node_name)
        .ok_or(StepError::LogicalError {
            message: format!("Node '{wallet_node_name}' for wallet '{wallet_name}' not found"),
        })?;
    let consensus = node.started_node.client.consensus_info().await?;
    let mut current = consensus.tip;

    // Get all blocks from the current tip walking backwards, but stop as soon as
    // we hit a header for which we already have cached wallet state.
    let mut tail_blocks = Vec::new();
    let mut reached_chain_start = false;
    let mut cached_ancestor_header_id: Option<String> = None;
    loop {
        let Some(block) = node.started_node.client.storage_block(&current).await? else {
            reached_chain_start = true;
            break;
        };

        let header_id = block.header().id().to_string();

        // If we have cached state for this wallet at this header, we can stop going
        // further back. NOTE: cache represents the post-state after evaluating
        // this block.
        if let Some(wallet_token_map) = world.wallet_tokens_per_block.get(&header_id)
            && wallet_token_map.utxos_per_wallet.contains_key(wallet_name)
        {
            owned.clear();
            for utxo in &wallet_token_map.utxos_per_wallet[wallet_name] {
                owned.insert(utxo.id(), *utxo);
            }
            cached_ancestor_header_id = Some(header_id);
            break;
        }

        let parent = block.header().parent();
        tail_blocks.push(block);
        current = parent;
    }
    tail_blocks.reverse();

    // Evaluate the tail blocks forward to reconstruct the wallet state at the tip.
    let (base_height, height_prefix) = get_last_known_height(
        &world.node_header_heights,
        cached_ancestor_header_id.as_ref(),
        wallet_node_name,
        reached_chain_start,
    );
    for (i, block) in tail_blocks.iter().enumerate() {
        let header_id = block.header().id().to_string();
        let height = base_height + i as u64;
        record_header_height(
            &mut world.node_header_heights,
            wallet_node_name,
            &header_id,
            height,
        );

        if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
            info!(
                target: TARGET,
                "Evaluating block {height_prefix}{height} for `{wallet_name}/{wallet_node_name}`: \
                {}, transactions len: {}",
                header_id,
                block.transactions().len(),
            );
        }

        for tx in block.transactions() {
            // Unspent outputs
            for utxo in tx.mantle_tx.ledger_tx.utxos() {
                if utxo.note.pk == wallet_pk {
                    owned.insert(utxo.id(), utxo);
                    if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                        info!(
                            target: TARGET,
                            "Found UTXO for `{wallet_name}/{wallet_node_name}`: value: {}, id: {:?}",
                            utxo.note.value,
                            utxo.id(),
                        );
                    }
                }
            }

            // Spent outputs
            for spent in &tx.mantle_tx.ledger_tx.inputs {
                if owned.remove(spent).is_some() {
                    if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                        info!(
                            target: TARGET,
                            "Found spent UTXO for `{wallet_name}/{wallet_node_name}`: id: {:?}",
                            spent
                        );
                    }
                    if let Some(encumbered) = world.wallet_encumbered_tokens.get_mut(wallet_name) {
                        encumbered.retain(|u| u.id() != *spent);
                    }
                }
            }
        }

        // Update wallet tokens cache
        let entry = world
            .wallet_tokens_per_block
            .entry(header_id.clone())
            .or_insert_with(|| WalletTokenMap {
                header_id: header_id.clone(),
                utxos_per_wallet: HashMap::new(),
            });
        entry
            .utxos_per_wallet
            .insert(wallet_name.to_owned(), owned.values().copied().collect());
    }

    Ok(owned.into_values().collect())
}

fn wallet_state_from_utxos(utxos: Vec<Utxo>) -> lb_wallet::WalletState {
    let mut utxo_map = rpds::HashTrieMapSync::new_sync();
    let mut pk_index = rpds::HashTrieMapSync::new_sync();

    for utxo in utxos {
        let note_id = utxo.id();
        let pk = utxo.note.pk;
        utxo_map = utxo_map.insert(note_id, utxo);

        let note_set = pk_index
            .get(&pk)
            .cloned()
            .unwrap_or_else(rpds::HashTrieSetSync::new_sync)
            .insert(note_id);
        pk_index = pk_index.insert(pk, note_set);
    }

    lb_wallet::WalletState {
        utxos: utxo_map,
        pk_index,
    }
}

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
