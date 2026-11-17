use lb_config::{
    consensus::SdpFundingConfig,
    funding::{fill_leader_stake, fund_wallets},
    sdp::create_sdp_configs,
};
use lb_core::{
    block::genesis::GenesisBlock,
    mantle::GenesisTime,
    sdp::{Locator, ServiceType},
};
use lb_key_management_system_service::keys::{Key, ZkKey};

use super::{
    Config,
    node_configs::{
        blend::GeneralBlendConfig,
        consensus::{
            BLEND_DECLARATION_AMOUNT, ProviderInfo, create_genesis_block_with_declarations,
        },
    },
};

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

    let leader_keys = general_configs
        .iter()
        .map(|general| general.consensus_config.known_key.to_public_key())
        .collect::<Vec<_>>();
    let funding_keys = general_configs
        .iter()
        .map(|general| general.consensus_config.funding_pk)
        .collect::<Vec<_>>();
    let mut transfer_op = genesis_block.genesis_tx().distribution().clone();
    fund_wallets(
        &mut transfer_op,
        &funding_keys,
        wallet_accounts,
        sdp_funding_config,
    );

    let blend_configs = general_configs
        .iter()
        .map(|general| general.blend_config.clone())
        .collect::<Vec<GeneralBlendConfig>>();

    let mut providers = Vec::with_capacity(blend_configs.len());
    for (blend_conf, private_key, secret_zk_key) in blend_configs.iter().take(n_blend_core_nodes) {
        providers.push(ProviderInfo {
            service_type: ServiceType::BlendNetwork,
            provider_sk: private_key.clone(),
            zk_sk: secret_zk_key.clone(),
            locator: Locator::new_unchecked(blend_conf.core.backend.listening_address.clone()),
            amount: BLEND_DECLARATION_AMOUNT,
        });
    }

    let declared = providers.iter().map(|provider| provider.amount).sum();
    fill_leader_stake(&mut transfer_op, &leader_keys, declared);

    let genesis_block =
        create_genesis_block_with_declarations(transfer_op, &providers, test_context, genesis_time);

    let sdp_configs = create_sdp_configs(genesis_block.genesis_tx(), general_configs.len());
    for (general, sdp_config) in general_configs.iter_mut().zip(sdp_configs) {
        general.sdp_config = sdp_config;
        for (secret_key, _) in wallet_accounts {
            let key = Key::Zk(secret_key.clone());
            let key_id = key_id_for_preload_backend(&key);
            general.kms_config.backend.keys.entry(key_id).or_insert(key);
        }
    }

    genesis_block
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{ops::OpRef, traits::MantleTx as _};
    use lb_sdp_service::DeclarationConfig;

    use crate::{
        DeploymentBuilder, TopologyConfig,
        configs::{
            deployment::SdpFundingConfig,
            wallet::{WalletAccount, WalletConfig},
        },
        internal::apply_wallet_config_to_deployment,
    };

    #[test]
    fn late_wallet_funding_keeps_provider_references_and_sdp_funds() {
        let funding = SdpFundingConfig::new(10_003, 124);
        let mut plan = DeploymentBuilder::new(
            TopologyConfig::with_node_numbers(2).with_sdp_funding_config(funding),
        )
        .build()
        .unwrap();
        let wallets = WalletConfig::new(
            (0..4)
                .map(|i| WalletAccount::deterministic(i, 100, false).unwrap())
                .collect(),
        );
        apply_wallet_config_to_deployment(&mut plan, &wallets);

        let genesis = plan.config().genesis_block.as_ref().unwrap();
        let transfer = genesis.genesis_tx().distribution();
        assert_eq!(transfer.len(), 254);
        let declarations = genesis
            .genesis_tx()
            .op_refs()
            .into_iter()
            .filter_map(|op| match op {
                OpRef::SDPDeclare(declaration) => Some(declaration),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (i, node) in plan.nodes().iter().enumerate() {
            let consensus = &node.general.consensus_config;
            let outputs = transfer
                .iter()
                .filter(|note| note.public_key == consensus.funding_pk)
                .collect::<Vec<_>>();
            assert_eq!(outputs.len(), 124);
            assert_eq!(
                outputs.iter().map(|note| note.value).sum::<u64>(),
                funding.total_value_per_node
            );
            assert!(declarations[i].inputs.is_empty());
            assert_eq!(
                node.general
                    .sdp_config
                    .declaration
                    .as_ref()
                    .map(DeclarationConfig::id),
                Some(declarations[i].id())
            );
        }
    }
}
