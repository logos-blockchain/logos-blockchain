pub mod api;
pub mod blend;
pub mod consensus;
pub mod deployment;
pub mod funding;
pub mod kms;
pub mod logos;
pub mod network;
pub mod node;
pub mod preparation;
pub mod sdp;
pub mod time;
pub mod tracing;
mod unique;

use core::time::Duration;
use std::sync::LazyLock;

use lb_core::{block::genesis::GenesisBlock, mantle::GenesisTime};
pub use logos::{GeneralConfig, create_general_configs_from_prepared_network};
use network::NetworkParams;
use rand::{Rng as _, thread_rng};

use crate::{
    consensus::{SHORT_PROLONGED_BOOTSTRAP_PERIOD, SdpFundingConfig},
    preparation::prepare_network,
};

/// Global flag indicating whether debug tracing configuration is enabled to
/// send traces to local grafana stack.
pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("LOGOS_BLOCKCHAIN_TESTS_TRACING")
        .is_ok_and(|val| val.eq_ignore_ascii_case("true"))
});

#[must_use]
pub fn create_general_configs(
    n_nodes: usize,
    test_context: Option<&str>,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConfig>, GenesisBlock) {
    create_general_configs_with_network(
        n_nodes,
        &NetworkParams::default(),
        test_context,
        genesis_time,
    )
}

#[must_use]
pub fn create_general_configs_with_network(
    n_nodes: usize,
    network_params: &NetworkParams,
    test_context: Option<&str>,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConfig>, GenesisBlock) {
    create_general_configs_with_blend_core_subset(
        n_nodes,
        n_nodes,
        network_params,
        test_context,
        genesis_time,
    )
}

#[must_use]
pub fn create_general_configs_with_blend_core_subset(
    n_nodes: usize,
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
    test_context: Option<&str>,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConfig>, GenesisBlock) {
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
        genesis_time,
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
    genesis_time: GenesisTime,
) -> (Vec<GeneralConfig>, GenesisBlock) {
    create_general_configs_from_ids_with_additional_wallet_outputs(
        ids,
        blend_ports,
        n_blend_core_nodes,
        network_params,
        prolonged_bootstrap_period,
        test_context,
        0,
        genesis_time,
    )
}

#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Configuration wrapper passes through all required deployment inputs."
)]
pub fn create_general_configs_from_ids_with_additional_wallet_outputs(
    ids: &[[u8; 32]],
    blend_ports: &[u16],
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
    prolonged_bootstrap_period: Duration,
    test_context: Option<&str>,
    additional_wallet_outputs: usize,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConfig>, GenesisBlock) {
    create_general_configs_from_ids_with_additional_wallet_outputs_and_sdp_funding_config(
        ids,
        blend_ports,
        n_blend_core_nodes,
        network_params,
        prolonged_bootstrap_period,
        test_context,
        additional_wallet_outputs,
        SdpFundingConfig::default(),
        genesis_time,
    )
}

#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Configuration wrapper passes through all required deployment inputs."
)]
pub fn create_general_configs_from_ids_with_additional_wallet_outputs_and_sdp_funding_config(
    ids: &[[u8; 32]],
    blend_ports: &[u16],
    n_blend_core_nodes: usize,
    network_params: &NetworkParams,
    prolonged_bootstrap_period: Duration,
    test_context: Option<&str>,
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConfig>, GenesisBlock) {
    let prepared = prepare_network(
        ids,
        blend_ports,
        n_blend_core_nodes,
        additional_wallet_outputs,
        &[],
        sdp_funding_config,
        test_context,
        genesis_time,
    );
    let configs = create_general_configs_from_prepared_network(
        &prepared,
        network_params,
        prolonged_bootstrap_period,
        &[],
    );
    (configs, prepared.genesis)
}
