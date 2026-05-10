use std::collections::HashSet;

use lb_core::{
    block::genesis::GenesisBlock,
    mantle::{GenesisTx as _, Note},
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

pub fn apply_wallet_genesis_overrides(
    general_configs: &mut [Config],
    genesis_block: &GenesisBlock,
    n_blend_core_nodes: usize,
    wallet_accounts: &[(ZkKey, u64)],
    key_id_for_preload_backend: impl Fn(&Key) -> String,
    test_context: Option<&str>,
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

    let mut transfer_op = genesis_block
        .transactions()
        .next()
        .expect("Genesis block should have a genesis tx")
        .genesis_transfer()
        .clone();
    for output in transfer_op.outputs.as_mut() {
        if leader_keys.contains(&output.pk) {
            output.value = leader_stake;
        }
    }
    for (secret_key, value) in wallet_accounts {
        transfer_op
            .outputs
            .as_mut()
            .push(Note::new(*value, secret_key.to_public_key()));
    }

    let genesis_block =
        create_genesis_block_with_declarations(transfer_op, providers, test_context);

    for general in general_configs.iter_mut() {
        for (secret_key, _) in wallet_accounts {
            let key = Key::Zk(secret_key.clone());
            let key_id = key_id_for_preload_backend(&key);
            general.kms_config.backend.keys.entry(key_id).or_insert(key);
        }
    }

    genesis_block
}
