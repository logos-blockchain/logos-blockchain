#![allow(
    clippy::redundant_pub_crate,
    reason = "Imported shared config modules expose pub(crate) constants."
)]

#[path = "../../../../src/topology/configs/api.rs"]
pub mod api;
#[path = "../../../../src/topology/configs/blend.rs"]
pub mod blend;
#[path = "../../../../src/topology/configs/consensus.rs"]
pub mod consensus;
#[path = "../../../../src/topology/configs/deployment.rs"]
pub mod deployment;
#[path = "../../../../src/topology/configs/network.rs"]
pub mod network;
#[path = "../../../../src/topology/configs/time.rs"]
pub mod time;
#[path = "../../../../src/topology/configs/tracing.rs"]
pub mod tracing;

use blend::GeneralBlendConfig;
use consensus::{GeneralConsensusConfig, ProviderInfo, create_genesis_tx_with_declarations};
use lb_core::{
    mantle::{GenesisTx as _, genesis_tx::GenesisTx},
    sdp::{Locator, ServiceType},
};
use lb_node::config::{KmsConfig, kms::serde::PreloadKmsBackendSettings};
use network::GeneralNetworkConfig;
use tracing::GeneralTracingConfig;

use self::{
    api::GeneralApiConfig, consensus::SHORT_PROLONGED_BOOTSTRAP_PERIOD, network::NetworkParams,
    time::GeneralTimeConfig,
};
use crate::common::kms::key_id_for_preload_backend;

#[derive(Clone)]
pub struct GeneralConfig {
    pub api_config: GeneralApiConfig,
    pub consensus_config: GeneralConsensusConfig,
    pub network_config: GeneralNetworkConfig,
    pub blend_config: GeneralBlendConfig,
    pub tracing_config: GeneralTracingConfig,
    pub time_config: GeneralTimeConfig,
    pub kms_config: KmsConfig,
}

#[must_use]
pub fn create_general_configs_from_ids(
    ids: &[[u8; 32]],
    blend_ports: &[u16],
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
) -> (Vec<GeneralConfig>, GenesisTx) {
    let n_nodes = ids.len();

    assert_eq!(
        ids.len(),
        blend_ports.len(),
        "blend_ports({}) must match ids({})",
        blend_ports.len(),
        ids.len()
    );
    assert!(
        n_blend_core_nodes <= ids.len(),
        "n_blend_core_nodes({n_blend_core_nodes}) must be <= ids.len()({})",
        ids.len()
    );

    let (consensus_configs, genesis_tx) =
        consensus::create_consensus_configs(ids, SHORT_PROLONGED_BOOTSTRAP_PERIOD);
    let network_configs = network::create_network_configs(ids, network_params);
    let api_configs = api::create_api_configs(ids);
    let blend_configs = blend::create_blend_configs(ids, blend_ports);
    let tracing_configs = tracing::create_tracing_configs(ids);
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
                note: consensus_configs[i].blend_note.clone(),
            },
        )
        .collect();

    let ledger_tx = genesis_tx.mantle_tx().ledger_tx.clone();
    let genesis_tx_with_declarations = create_genesis_tx_with_declarations(ledger_tx, providers);

    let kms_configs: Vec<_> = blend_configs
        .iter()
        .enumerate()
        .map(|(i, (blend_conf, private_key, zk_secret_key))| KmsConfig {
            backend: PreloadKmsBackendSettings {
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
                    (
                        key_id_for_preload_backend(&consensus_configs[i].funding_sk.clone().into()),
                        consensus_configs[i].funding_sk.clone().into(),
                    ),
                ]
                .into(),
            },
        })
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
