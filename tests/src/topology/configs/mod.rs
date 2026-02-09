pub mod api;
pub mod blend;
pub mod consensus;
pub mod deployment;
pub mod network;
pub mod time;
pub mod tracing;

use blend::GeneralBlendConfig;
use consensus::{GeneralConsensusConfig, ProviderInfo, create_genesis_tx_with_declarations};
use lb_core::{
    mantle::{GenesisTx as _, genesis_tx::GenesisTx},
    sdp::{Locator, ServiceType},
};
use lb_key_management_system_service::backend::preload::PreloadKMSBackendSettings;
use lb_utils::net::get_available_udp_port;
use network::GeneralNetworkConfig;
use rand::{Rng as _, thread_rng};
use tracing::GeneralTracingConfig;

use crate::{
    common::kms::key_id_for_preload_backend,
    topology::configs::{
        api::GeneralApiConfig, consensus::SHORT_PROLONGED_BOOTSTRAP_PERIOD, network::NetworkParams,
        time::GeneralTimeConfig,
    },
};

#[derive(Clone)]
pub struct GeneralConfig {
    pub api_config: GeneralApiConfig,
    pub consensus_config: GeneralConsensusConfig,
    pub network_config: GeneralNetworkConfig,
    pub blend_config: GeneralBlendConfig,
    pub tracing_config: GeneralTracingConfig,
    pub time_config: GeneralTimeConfig,
    pub kms_config: PreloadKMSBackendSettings,
}

#[must_use]
pub fn create_general_configs(n_nodes: usize) -> (Vec<GeneralConfig>, GenesisTx) {
    create_general_configs_with_network(n_nodes, &NetworkParams::default())
}

#[must_use]
pub fn create_general_configs_with_network(
    n_nodes: usize,
    network_params: &NetworkParams,
) -> (Vec<GeneralConfig>, GenesisTx) {
    create_general_configs_with_blend_core_subset(n_nodes, n_nodes, network_params)
}

#[must_use]
pub fn create_general_configs_with_blend_core_subset(
    n_nodes: usize,
    // TODO: Instead of this, define a config struct for each node.
    // That would be also useful for non-even token distributions: https://github.com/logos-blockchain/logos-blockchain/issues/1888
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
) -> (Vec<GeneralConfig>, GenesisTx) {
    assert!(
        n_blend_core_nodes <= n_nodes,
        "n_blend_core_nodes({n_blend_core_nodes}) must be less than or equal to n_nodes({n_nodes})",
    );

    // Blend relies on each node declaring a different ZK public key, so we need
    // different IDs to generate different keys.
    let mut ids: Vec<_> = (0..n_nodes).map(|i| [i as u8; 32]).collect();
    let mut blend_ports = vec![];

    for id in &mut ids {
        thread_rng().fill(id);
        blend_ports.push(get_available_udp_port().unwrap());
    }

    let (consensus_configs, genesis_tx) =
        consensus::create_consensus_configs(&ids, SHORT_PROLONGED_BOOTSTRAP_PERIOD);
    let network_configs = network::create_network_configs(&ids, network_params);
    let api_configs = api::create_api_configs(&ids);
    let blend_configs = blend::create_blend_configs(&ids, &blend_ports);
    let tracing_configs = tracing::create_tracing_configs(&ids);
    let time_config = time::default_time_config();

    let providers: Vec<_> = blend_configs
        .iter()
        .enumerate()
        .take(n_blend_core_nodes)
        .map(
            |(i, (blend_conf, private_key, secret_zk_key))| ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk: private_key.clone(),
                zk_sk: secret_zk_key.clone(),
                locator: Locator(blend_conf.core.backend.listening_address.clone()),
                note: consensus_configs[0].blend_notes[i].clone(),
            },
        )
        .collect();
    let ledger_tx = genesis_tx.mantle_tx().ledger_tx.clone();
    let genesis_tx_with_declarations = create_genesis_tx_with_declarations(ledger_tx, providers);

    // Set note keys and Blend keys in KMS of each node config.
    let kms_configs: Vec<_> = blend_configs
        .iter()
        .enumerate()
        .map(
            |(i, (blend_conf, private_key, zk_secret_key))| PreloadKMSBackendSettings {
                keys: [
                    (
                        blend_conf.non_ephemeral_signing_key_id.clone(),
                        private_key.clone().into(),
                    ),
                    (
                        blend_conf.core.zk.secret_key_kms_id.clone(),
                        zk_secret_key.clone().into(),
                    ),
                    (
                        key_id_for_preload_backend(&consensus_configs[i].known_key.clone().into()),
                        consensus_configs[i].known_key.clone().into(),
                    ),
                    // SDP funding secret key - used by wallet for signing SDP transactions
                    (
                        key_id_for_preload_backend(&consensus_configs[i].funding_sk.clone().into()),
                        consensus_configs[i].funding_sk.clone().into(),
                    ),
                ]
                .into(),
            },
        )
        .collect();

    let mut general_configs = vec![];

    for i in 0..n_nodes {
        general_configs.push(GeneralConfig {
            api_config: api_configs[i].clone(),
            consensus_config: consensus_configs[i].clone(),
            network_config: network_configs[i].clone(),
            blend_config: blend_configs[i].clone(),
            tracing_config: tracing_configs[i].clone(),
            time_config: time_config.clone(),
            kms_config: kms_configs[i].clone(),
        });
    }

    (general_configs, genesis_tx_with_declarations)
}
