//! Native Logos service settings, rendered from prepared network material.

use core::time::Duration;

use lb_key_management_system_service::keys::Key;
use lb_node::config::KmsConfig;

use crate::{
    api::{self, GeneralApiConfig},
    blend::{self, GeneralBlendConfig},
    consensus::{self, GeneralConsensusConfig},
    kms::create_kms_configs,
    network::{self, GeneralNetworkConfig, NetworkParams},
    preparation::PreparedNetwork,
    sdp::{GeneralSdpConfig, create_sdp_configs},
    time::{GeneralTimeConfig, set_time_config},
    tracing::{self, GeneralTracingConfig},
};

#[derive(Clone)]
pub struct GeneralConfig {
    pub api_config: GeneralApiConfig,
    pub consensus_config: GeneralConsensusConfig,
    pub network_config: GeneralNetworkConfig,
    pub blend_config: GeneralBlendConfig,
    pub tracing_config: GeneralTracingConfig,
    pub time_config: GeneralTimeConfig,
    pub kms_config: KmsConfig,
    pub sdp_config: GeneralSdpConfig,
}

/// Renders Logos service configuration from an already prepared network.
/// This phase does not generate or change genesis, funding or participant keys.
#[must_use]
pub fn create_general_configs_from_prepared_network(
    prepared: &PreparedNetwork,
    network_params: &NetworkParams,
    prolonged_bootstrap_period: Duration,
    wallet_keys: &[Key],
) -> Vec<GeneralConfig> {
    let ids = &prepared.ids;
    let blend_ports = &prepared.blend_ports;
    let n_nodes = ids.len();
    assert_eq!(n_nodes, prepared.consensus.regular_note_keys.len());
    assert_eq!(n_nodes, blend_ports.len());
    let consensus_configs =
        consensus::configs_from_material(&prepared.consensus, prolonged_bootstrap_period);
    let network_configs = network::create_network_configs(ids, network_params);
    let api_configs = api::create_api_configs(ids);
    let blend_configs = blend::create_blend_configs(ids, blend_ports);
    let tracing_configs = tracing::create_tracing_configs(ids);
    let time_config = set_time_config();
    let sdp_configs = create_sdp_configs(prepared.genesis.genesis_tx(), n_nodes);
    let kms_configs = create_kms_configs(&blend_configs, &consensus_configs, Some(wallet_keys));

    (0..n_nodes)
        .map(|i| GeneralConfig {
            api_config: api_configs[i].clone(),
            consensus_config: consensus_configs[i].clone(),
            network_config: network_configs[i].clone(),
            blend_config: blend_configs[i].clone(),
            tracing_config: tracing_configs[i].clone(),
            time_config: time_config.clone(),
            kms_config: kms_configs[i].clone(),
            sdp_config: sdp_configs[i].clone(),
        })
        .collect()
}
