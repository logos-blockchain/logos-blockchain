use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use lb_testing_framework::NodeHttpClient;

use super::{
    config::{ForkGroupScannerConfig, ScannerSeed, SharedBestNodeSelector},
    state::{ForkGroupScannerState, SharedWalletScannerState},
};
use crate::{
    common::wallet::TrackedWalletKeys,
    cucumber::{
        error::StepError, fee_reserve::SCENARIO_FEE_ACCOUNT_NAME,
        wallet::best_node::get_best_node_info_from_clients, world::CucumberWorld,
    },
};

/// Default interval between scanner polls.
const DEFAULT_SCANNER_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Default number of applied blocks between wallet-state publications.
const DEFAULT_SCANNER_RANGE_BATCH_SIZE: u64 = 1_000;

#[expect(
    clippy::too_many_lines,
    reason = "Group config assembly is centralized so scanner startup stays in one place."
)]
#[expect(
    clippy::type_complexity,
    reason = "The grouped accumulator is transient and clearer inline than with extra wrapper types."
)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "The shared scanner state is cloned into spawned runtime configs."
)]
/// Build scanner runtime configs for all fork groups with tracked wallets.
pub fn build_fork_group_scanner_configs(
    world: &CucumberWorld,
    scanner_state: SharedWalletScannerState,
) -> Result<Vec<ForkGroupScannerConfig>, StepError> {
    let mut groups: BTreeMap<
        String,
        (
            Vec<String>,
            Vec<TrackedWalletKeys>,
            BTreeMap<String, String>,
        ),
    > = BTreeMap::new();

    if world.node_groups.is_empty() {
        groups.insert(
            String::new(),
            (world.all_node_names(), Vec::new(), BTreeMap::new()),
        );
    } else {
        for (group_id, nodes) in &world.node_groups {
            groups.insert(
                group_id.clone(),
                (nodes.iter().cloned().collect(), Vec::new(), BTreeMap::new()),
            );
        }
    }

    for wallet in world.wallet_info.values() {
        let group_id = if world.node_groups.is_empty() {
            world
                .node_to_group
                .get(&wallet.node_name)
                .cloned()
                .unwrap_or_default()
        } else {
            world
                .node_to_group
                .get(&wallet.node_name)
                .cloned()
                .ok_or_else(|| StepError::LogicalError {
                    message: format!(
                        "wallet scanner grouping misconfiguration: wallet '{}' is bound to node '{}' \
                         but that node is not present in `node_to_group` while explicit `node_groups` \
                         are configured",
                        wallet.wallet_name, wallet.node_name
                    ),
                })?
        };

        let entry = groups
            .entry(group_id)
            .or_insert_with(|| (Vec::new(), Vec::new(), BTreeMap::new()));
        entry.1.push(TrackedWalletKeys::new(
            wallet.wallet_name.clone(),
            [wallet.public_key()?],
        ));
        entry
            .2
            .insert(wallet.wallet_name.clone(), wallet.node_name.clone());
    }

    if let Some(fee_wallet_account) = world.fee_state.wallet_account.clone() {
        let mut assigned = false;
        for (nodes, wallet_keys, wallet_to_node) in groups.values_mut() {
            if wallet_keys.is_empty() {
                continue;
            }

            if let Some(node_name) = nodes
                .first()
                .cloned()
                .or_else(|| world.all_node_names().first().cloned())
            {
                wallet_keys.push(TrackedWalletKeys::new(
                    SCENARIO_FEE_ACCOUNT_NAME,
                    [fee_wallet_account.public_key()],
                ));
                wallet_to_node.insert(SCENARIO_FEE_ACCOUNT_NAME.to_owned(), node_name);
                assigned = true;
            }
        }
        if !assigned {
            let entry = groups
                .entry(String::new())
                .or_insert_with(|| (world.all_node_names(), Vec::new(), BTreeMap::new()));
            entry.1.push(TrackedWalletKeys::new(
                SCENARIO_FEE_ACCOUNT_NAME,
                [fee_wallet_account.public_key()],
            ));
            if let Some(node_name) = entry.0.first().cloned() {
                entry
                    .2
                    .insert(SCENARIO_FEE_ACCOUNT_NAME.to_owned(), node_name);
            }
        }
    }

    let selector: SharedBestNodeSelector = Arc::new(
        |representative_wallet_name,
         group_nodes,
         node_clients,
         wallet_to_node,
         group_id,
         mut last_msg| {
            Box::pin(async move {
                let node_to_group = node_clients
                    .keys()
                    .map(|node_name| (node_name.clone(), group_id.clone()))
                    .collect::<BTreeMap<_, _>>();
                get_best_node_info_from_clients(
                    &representative_wallet_name,
                    &wallet_to_node,
                    &node_to_group,
                    &group_nodes,
                    &node_clients,
                    Some(&mut last_msg),
                )
                .await
            })
        },
    );

    let mut configs = Vec::new();
    for (group_id, (nodes, wallet_keys, wallet_to_node)) in groups {
        if wallet_keys.is_empty() {
            continue;
        }

        let representative_wallet_name = wallet_keys[0].wallet_id().to_string();
        let node_clients = nodes
            .iter()
            .filter_map(|node_name| {
                world
                    .nodes_info
                    .get(node_name)
                    .map(|node| (node_name.clone(), node.started_node.client.clone()))
            })
            .collect::<BTreeMap<String, NodeHttpClient>>();

        scanner_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .groups
            .insert(
                group_id.clone(),
                ForkGroupScannerState::new(
                    group_id.clone(),
                    representative_wallet_name.clone(),
                    wallet_keys.len(),
                ),
            );

        configs.push(ForkGroupScannerConfig {
            group_id,
            representative_wallet_name,
            seed: scanner_seed_for_group(world, &nodes, &wallet_keys)?,
            wallet_keys,
            node_clients,
            wallet_to_node,
            group_nodes: nodes,
            wallets: Arc::clone(&world.wallets),
            observed_transaction_hashes: Arc::clone(&world.observed_transaction_hashes),
            scanner_state: Arc::clone(&scanner_state),
            poll_interval: DEFAULT_SCANNER_POLL_INTERVAL,
            range_batch_size: DEFAULT_SCANNER_RANGE_BATCH_SIZE,
            genesis_utxos: world.genesis_block_utxos.clone(),
            best_node_selector: Arc::clone(&selector),
        });
    }

    Ok(configs)
}

fn scanner_seed_for_group(
    world: &CucumberWorld,
    nodes: &[String],
    wallet_keys: &[TrackedWalletKeys],
) -> Result<ScannerSeed, StepError> {
    let mut merged_seed = None;

    for seed in nodes
        .iter()
        .filter_map(|node_name| world.wallet_scanner_seeds.get(node_name))
        .map(|seed| seed.filtered_for_wallets(wallet_keys))
    {
        let ScannerSeed::Snapshot {
            wallet_utxos,
            tip,
            height,
            slot,
            source_node_names,
            rescan_blocks,
        } = seed
        else {
            continue;
        };

        let seed = merged_seed.get_or_insert_with(|| ScannerSeed::Snapshot {
            wallet_utxos: HashMap::default(),
            tip,
            height,
            slot,
            source_node_names: Vec::new(),
            rescan_blocks,
        });

        let ScannerSeed::Snapshot {
            wallet_utxos: merged_wallet_utxos,
            tip: merged_tip,
            height: merged_height,
            slot: merged_slot,
            source_node_names: merged_source_node_names,
            rescan_blocks: merged_rescan_blocks,
        } = seed
        else {
            unreachable!("merged seed is always a snapshot");
        };

        if *merged_tip != tip || *merged_height != height || *merged_slot != slot {
            return Err(StepError::LogicalError {
                message: format!(
                    "wallet scanner snapshot seeds for group have different chain positions: \
                     first={merged_tip}/{merged_height}/{merged_slot} next={tip}/{height}/{slot}",
                ),
            });
        }

        merged_wallet_utxos.extend(wallet_utxos);
        merged_source_node_names.extend(source_node_names);
        *merged_rescan_blocks = (*merged_rescan_blocks).max(rescan_blocks);
    }

    Ok(merged_seed.unwrap_or(ScannerSeed::Genesis))
}
