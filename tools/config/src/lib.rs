pub mod api;
pub mod blend;
pub mod consensus;
pub mod deployment;
pub mod kms;
pub mod network;
pub mod release;
pub mod sdp;
pub mod time;
pub mod tracing;
mod unique;

use core::time::Duration;
use std::sync::LazyLock;

use blend::GeneralBlendConfig;
use consensus::{GeneralConsensusConfig, ProviderInfo, SHORT_PROLONGED_BOOTSTRAP_PERIOD};
use lb_core::{
    mantle::{GenesisTx as _, genesis_tx::GenesisTx},
    sdp::{Locator, ServiceType},
};
use lb_node::config::KmsConfig;
use network::{GeneralNetworkConfig, NetworkParams};
use rand::{Rng as _, thread_rng};
use tracing::GeneralTracingConfig;

use crate::{
    api::GeneralApiConfig,
    consensus::create_genesis_tx_with_declarations,
    kms::create_kms_configs,
    sdp::{GeneralSdpConfig, create_sdp_configs},
    time::{GeneralTimeConfig, set_time_config},
};

/// Global flag indicating whether debug tracing configuration is enabled to
/// send traces to local grafana stack.
pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("LOGOS_BLOCKCHAIN_TESTS_TRACING")
        .is_ok_and(|val| val.eq_ignore_ascii_case("true"))
});

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

#[must_use]
pub fn create_general_configs(
    n_nodes: usize,
    test_context: Option<&str>,
) -> (Vec<GeneralConfig>, GenesisTx) {
    create_general_configs_with_network(n_nodes, &NetworkParams::default(), test_context)
}

#[must_use]
pub fn create_general_configs_with_network(
    n_nodes: usize,
    network_params: &NetworkParams,
    test_context: Option<&str>,
) -> (Vec<GeneralConfig>, GenesisTx) {
    create_general_configs_with_blend_core_subset(n_nodes, n_nodes, network_params, test_context)
}

#[must_use]
pub fn create_general_configs_with_blend_core_subset(
    n_nodes: usize,
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
    test_context: Option<&str>,
) -> (Vec<GeneralConfig>, GenesisTx) {
    assert!(
        n_blend_core_nodes <= n_nodes,
        "n_blend_core_nodes({n_blend_core_nodes}) must be less than or equal to n_nodes({n_nodes})",
    );

    let mut ids: Vec<_> = (0..n_nodes).map(|i| [i as u8; 32]).collect();
    let mut blend_ports = Vec::with_capacity(n_nodes);

    for id in &mut ids {
        thread_rng().fill(id);
        blend_ports.push(unique::get_reserved_available_udp_port().unwrap());
    }

    create_general_configs_from_ids(
        &ids,
        &blend_ports,
        n_blend_core_nodes,
        network_params,
        SHORT_PROLONGED_BOOTSTRAP_PERIOD,
        test_context,
    )
}

#[must_use]
pub fn create_general_configs_from_ids(
    ids: &[[u8; 32]],
    blend_ports: &[u16],
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
    prolonged_bootstrap_period: Duration,
    test_context: Option<&str>,
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
        "n_blend_core_nodes({n_blend_core_nodes}) must be less than or equal to ids.len()({})",
        ids.len()
    );

    let (consensus_configs, genesis_tx) =
        consensus::create_consensus_configs(ids, prolonged_bootstrap_period, test_context);
    let network_configs = network::create_network_configs(ids, network_params);
    let api_configs = api::create_api_configs(ids);
    let blend_configs = blend::create_blend_configs(ids, blend_ports);
    let tracing_configs = tracing::create_tracing_configs(ids);
    let time_config = set_time_config();

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
    let transfer_op = genesis_tx.genesis_transfer().clone();
    let genesis_tx_with_declarations =
        create_genesis_tx_with_declarations(transfer_op, providers, test_context);
    let sdp_configs = create_sdp_configs(&genesis_tx_with_declarations, n_nodes);
    let kms_configs = create_kms_configs(&blend_configs, &consensus_configs, None);

    let general_configs = (0..n_nodes)
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
        .collect();

    (general_configs, genesis_tx_with_declarations)
}
