use std::collections::{HashMap, HashSet};

use lb_config::consensus::{GENESIS_TRANSFER_OUTPUT_LIMIT, SdpFundingConfig};
use lb_core::{
    block::genesis::GenesisBlock,
    mantle::{GenesisTime, Note, ledger::Outputs},
    sdp::{Locator, ServiceType},
};
use lb_key_management_system_service::keys::{Key, ZkKey};

use super::{
    Config,
    node_configs::{
        blend::GeneralBlendConfig,
        consensus::{ProviderInfo, create_genesis_block_with_declarations},
    },
};

#[must_use]
pub fn leader_stake_amount(total_wallet_funds: u64, n_participants: usize) -> u64 {
    if total_wallet_funds == 0 {
        return 100_000;
    }

    let n = n_participants.max(1) as u64;
    let scaled = total_wallet_funds
        .saturating_mul(10)
        .saturating_div(n)
        .max(1);
    scaled.max(100_000)
}

fn fit_sdp_funding_outputs_to_genesis_capacity(
    transfer_op: &mut lb_core::mantle::ops::transfer::TransferOp,
    general_configs: &mut [Config],
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
) {
    let n_participants = general_configs.len();
    let funding_keys = general_configs
        .iter()
        .map(|general| general.consensus_config.funding_pk)
        .collect::<HashSet<_>>();
    let current_sdp_outputs = transfer_op
        .outputs
        .iter()
        .filter(|note| funding_keys.contains(&note.pk))
        .count();
    let non_sdp_outputs = transfer_op.outputs.len() - current_sdp_outputs;
    let required_non_sdp_outputs = non_sdp_outputs
        .checked_add(additional_wallet_outputs)
        .expect("genesis output count overflow while fitting SDP funding outputs");
    let available_for_sdp = GENESIS_TRANSFER_OUTPUT_LIMIT
        .checked_sub(required_non_sdp_outputs)
        .unwrap_or_else(|| {
            panic!(
                "genesis transfer output capacity exhausted before SDP funding outputs: limit={GENESIS_TRANSFER_OUTPUT_LIMIT}, non_sdp_outputs={non_sdp_outputs}, additional_wallet_outputs={additional_wallet_outputs}",
            )
        });
    let max_sdp_notes_per_node = available_for_sdp / n_participants;
    let selected_sdp_notes_per_node =
        max_sdp_notes_per_node.min(sdp_funding_config.target_notes_per_node);
    let required_sdp_outputs = n_participants
        .checked_mul(selected_sdp_notes_per_node)
        .expect("SDP funding output count overflow while fitting genesis capacity");

    assert!(
        selected_sdp_notes_per_node > 0,
        "genesis transfer output capacity cannot provide one SDP funding note per node: limit={GENESIS_TRANSFER_OUTPUT_LIMIT}, node_count={n_participants}, non_sdp_outputs={non_sdp_outputs}, additional_wallet_outputs={additional_wallet_outputs}",
    );
    assert!(
        current_sdp_outputs >= required_sdp_outputs,
        "genesis contains too few SDP funding outputs for the selected split: current={current_sdp_outputs}, required={required_sdp_outputs}",
    );

    let mut retained_per_key = HashMap::new();
    if current_sdp_outputs > required_sdp_outputs {
        let retained_notes = transfer_op
            .outputs
            .iter()
            .copied()
            .filter_map(|mut note| {
                if !funding_keys.contains(&note.pk) {
                    return Some(note);
                }

                let retained = retained_per_key.entry(note.pk).or_insert(0);
                if *retained >= selected_sdp_notes_per_node {
                    return None;
                }
                note.value = sdp_funding_note_value(
                    sdp_funding_config,
                    *retained,
                    selected_sdp_notes_per_node,
                );
                *retained += 1;
                Some(note)
            })
            .collect::<Vec<_>>();
        transfer_op.outputs = Outputs::try_new(retained_notes)
            .expect("trimmed genesis transfer outputs must fit the output bound");
    } else {
        for note in &mut transfer_op.outputs {
            if funding_keys.contains(&note.pk) {
                let retained = retained_per_key.entry(note.pk).or_insert(0);
                note.value = sdp_funding_note_value(
                    sdp_funding_config,
                    *retained,
                    selected_sdp_notes_per_node,
                );
                *retained += 1;
            }
        }
    }

    if current_sdp_outputs > required_sdp_outputs {
        for (output_index, note) in transfer_op.outputs.iter().enumerate() {
            if let Some(general) = general_configs
                .iter_mut()
                .find(|general| general.consensus_config.blend_note.pk == note.pk)
            {
                let utxo = transfer_op
                    .utxo_by_index(output_index)
                    .expect("genesis transfer output index must exist");
                general.consensus_config.blend_note.output_index = output_index;
                general.consensus_config.blend_note.note_id = utxo.id();
            }
        }
    }
}

fn sdp_funding_note_value(
    sdp_funding_config: SdpFundingConfig,
    note_index: usize,
    note_count: usize,
) -> u64 {
    let note_count = u64::try_from(note_count).expect("SDP funding split count should fit in u64");
    let base_value = sdp_funding_config.total_value_per_node / note_count;
    let remainder = sdp_funding_config.total_value_per_node % note_count;
    base_value
        + u64::from(
            u64::try_from(note_index).expect("SDP note index should fit in u64") < remainder,
        )
}

#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Genesis postprocessing passes through all deployment inputs."
)]
pub fn apply_wallet_genesis_overrides(
    general_configs: &mut [Config],
    genesis_block: &GenesisBlock,
    n_blend_core_nodes: usize,
    wallet_accounts: &[(ZkKey, u64)],
    key_id_for_preload_backend: impl Fn(&Key) -> String,
    test_context: Option<&str>,
    sdp_funding_config: SdpFundingConfig,
    genesis_time: GenesisTime,
) -> GenesisBlock {
    if wallet_accounts.is_empty() {
        return genesis_block.clone();
    }

    if general_configs.is_empty() {
        return genesis_block.clone();
    }

    let n_participants = general_configs.len();
    let total_wallet_funds = wallet_accounts.iter().map(|(_, value)| *value).sum::<u64>();
    let leader_stake = leader_stake_amount(total_wallet_funds, n_participants);

    let leader_keys = general_configs
        .iter()
        .map(|general| general.consensus_config.known_key.to_public_key())
        .collect::<HashSet<_>>();

    let blend_configs = general_configs
        .iter()
        .map(|general| general.blend_config.clone())
        .collect::<Vec<GeneralBlendConfig>>();

    let mut providers = Vec::with_capacity(blend_configs.len());
    for (idx, (blend_conf, private_key, secret_zk_key)) in
        blend_configs.iter().enumerate().take(n_blend_core_nodes)
    {
        providers.push(ProviderInfo {
            service_type: ServiceType::BlendNetwork,
            provider_sk: private_key.clone(),
            zk_sk: secret_zk_key.clone(),
            locator: Locator::new_unchecked(blend_conf.core.backend.listening_address.clone()),
            note: general_configs[idx].consensus_config.blend_note.clone(),
        });
    }

    let mut transfer_op = genesis_block.genesis_tx().transfer().operation().clone();
    fit_sdp_funding_outputs_to_genesis_capacity(
        &mut transfer_op,
        general_configs,
        wallet_accounts.len(),
        sdp_funding_config,
    );
    for output in &mut transfer_op.outputs {
        if leader_keys.contains(&output.pk) {
            output.value = leader_stake;
        }
    }
    for (secret_key, value) in wallet_accounts {
        transfer_op
            .outputs
            .try_push(Note::new(*value, secret_key.to_public_key()))
            .expect("wallet account outputs must fit transfer output bounds");
    }

    let genesis_block =
        create_genesis_block_with_declarations(transfer_op, providers, test_context, genesis_time);

    for general in general_configs.iter_mut() {
        for (secret_key, _) in wallet_accounts {
            let key = Key::Zk(secret_key.clone());
            let key_id = key_id_for_preload_backend(&key);
            general.kms_config.backend.keys.entry(key_id).or_insert(key);
        }
    }

    genesis_block
}
