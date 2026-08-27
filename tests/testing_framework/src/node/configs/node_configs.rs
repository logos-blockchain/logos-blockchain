#![allow(
    clippy::redundant_pub_crate,
    reason = "Imported shared config modules expose pub(crate) constants."
)]

use std::time::Duration;

pub use lb_config::GeneralConfig;
use lb_config::consensus::SdpFundingConfig;
pub(crate) use lb_config::{api, blend, consensus, network, sdp, time, tracing};
use lb_core::{block::genesis::GenesisBlock, mantle::GenesisTime};
use network::NetworkParams;

const PROLONGED_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(5);

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
    test_context: Option<&str>,
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConfig>, GenesisBlock) {
    lb_config::create_general_configs_from_ids_with_additional_wallet_outputs_and_sdp_funding_config(
        ids,
        blend_ports,
        n_blend_core_nodes,
        network_params,
        PROLONGED_BOOTSTRAP_PERIOD,
        test_context,
        additional_wallet_outputs,
        sdp_funding_config,
        genesis_time,
    )
}
